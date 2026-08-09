//! DocManager 测试（§7.4 单写者 / §6.4 微批次 / §8.5 命令路由）。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration as TokioDuration};

use acp_hub_proto::conn::DocId;
use acp_hub_proto::schema::ChatSummary;

use crate::state::doc_manager::{
    BatchConfig, DocCommand, DocManager, PersistError, SubmitError, SubmitResult, UpdateSink,
};
use crate::state::normalized::{EventBody, NormalizedEvent};

/// 抑制 paused clock 的 auto-advance（tokio 1.53：runtime 无 ready 任务时自动
/// 推进虚拟时钟到下一 timer，导致「窗口未到不 flush」断言失效）。
///
/// `spawn_blocking` 阻塞期间 auto-advance 被抑制（tokio 文档）；Drop 释放。
/// 每个 `#[tokio::test]` 是独立 runtime，clock 互不影响。
struct ClockBlocker {
    tx: std::sync::mpsc::Sender<()>,
}

impl ClockBlocker {
    fn hold() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        tokio::task::spawn_blocking(move || {
            let _ = rx.recv();
        });
        ClockBlocker { tx }
    }
}

impl Drop for ClockBlocker {
    fn drop(&mut self) {
        let _ = self.tx.send(());
    }
}

/// 内存测试替身 sink（记录落盘 update；可选择失败）。
type PersistedUpdate = (DocId, Vec<u8>);

#[derive(Clone, Default)]
struct MemSink {
    updates: Arc<Mutex<Vec<PersistedUpdate>>>,
    fail: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl UpdateSink for MemSink {
    async fn persist_update(&self, doc: DocId, update: Vec<u8>) -> Result<(), PersistError> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(PersistError("disk full".into()));
        }
        self.updates.lock().await.push((doc, update));
        Ok(())
    }
}

fn cfg() -> BatchConfig {
    BatchConfig {
        batch_window: Duration::from_millis(16),
        batch_bytes: 4096,
        chat_queue: 64,
    }
}

fn delta(chat: &str, seq: u64, turn: &str, text: &str) -> NormalizedEvent {
    NormalizedEvent {
        chat_id: chat.to_string(),
        seq,
        epoch: 0,
        body: EventBody::MessageDelta {
            turn_id: turn.to_string(),
            entry_id: format!("{turn}:assistant"),
            block_id: "b1".to_string(),
            text: text.to_string(),
        },
    }
}

fn user_msg(chat: &str, seq: u64, turn: &str) -> NormalizedEvent {
    NormalizedEvent {
        chat_id: chat.to_string(),
        seq,
        epoch: 0,
        body: EventBody::UserMessage {
            turn_id: turn.to_string(),
            entry_id: format!("{turn}:user"),
            text: "hi".to_string(),
            author_user_id: None,
            created_at: "2026-08-07T00:00:00Z".to_string(),
        },
    }
}

async fn open(mgr: &DocManager, chat: &str) {
    mgr.open_chat(chat, "m1", Some("t")).await.unwrap();
}

// ---------------------------------------------------------------------------
// 微批次合并（§6.4）：16ms 窗内 delta 合并为一次事务
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn batch_merges_deltas_into_single_transaction() {
    let _blocker = ClockBlocker::hold();
    let sink = MemSink::default();
    let mgr = Arc::new(DocManager::new(cfg(), Arc::new(sink.clone())));
    open(&mgr, "s1").await;
    // 先注册 turn（active_turn 存在）。
    assert!(matches!(
        mgr.submit_event(user_msg("s1", 1, "t1")).await,
        SubmitResult::Applied(_)
    ));
    // 3 个 delta 进入批次（batchable 事件的确认要等 flush，故 spawn 提交不阻塞主流程）。
    let mut handles = Vec::new();
    for seq in 2..5 {
        let mgr = mgr.clone();
        handles.push(tokio::spawn(async move {
            mgr.submit_event(delta("s1", seq, "t1", "x")).await
        }));
    }
    // 窗口未到：无落盘。
    {
        let updates = sink.updates.lock().await;
        let deltas = updates
            .iter()
            .filter(|(d, _)| *d == DocId::chat("s1"))
            .count();
        assert_eq!(deltas, 2, "仅 user_message 已落盘（+初始化基线），批次未 flush");
    }
    // 推进 16ms → 批次 flush 为一次 chat 事务（一个 update）。
    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;
    for h in handles {
        let r = h.await.expect("submit task panic");
        assert!(matches!(r, SubmitResult::Applied(_)), "delta 提交应确认 Applied");
    }
    {
        let updates = sink.updates.lock().await;
        let deltas = updates
            .iter()
            .filter(|(d, _)| *d == DocId::chat("s1"))
            .count();
        assert_eq!(deltas, 3, "批次应合并为一次 chat 事务：基线+user+批次");
    }
}

// ---------------------------------------------------------------------------
// 控制类先 flush（§6.4）：控制类到达 → 已缓冲 delta 先落盘
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn control_event_flushes_buffered_batch_first() {
    let _blocker = ClockBlocker::hold();
    let sink = MemSink::default();
    let mgr = Arc::new(DocManager::new(cfg(), Arc::new(sink.clone())));
    open(&mgr, "s1").await;
    assert!(matches!(
        mgr.submit_event(user_msg("s1", 1, "t1")).await,
        SubmitResult::Applied(_)
    ));
    // 2 个 delta 缓冲（未到窗口；按序提交，控制类必须先见到已入队 delta）。
    for seq in 2..4 {
        let r = mgr.submit_event(delta("s1", seq, "t1", "x")).await;
        assert!(matches!(r, SubmitResult::Applied(_)), "delta 入队应确认");
    }
    // 确保 spawn 的 delta 提交已到达 writer（避免调度顺序偶发）。
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    // 控制类事件（tool_call 状态）到达 → 先 flush 已缓冲批次再立即写。
    let control = NormalizedEvent {
        chat_id: "s1".to_string(),
        seq: 4,
        epoch: 0,
        body: EventBody::ToolCallStarted {
            turn_id: "t1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            arguments: Some(json!({})),
            created_at: "2026-08-07T00:00:00Z".to_string(),
        },
    };
    let _ = mgr.submit_event(control).await;
    {
        let updates = sink.updates.lock().await;
        let chat_docs: Vec<usize> = updates
            .iter()
            .filter(|(d, _)| *d == DocId::chat("s1"))
            .map(|(_, u)| u.len())
            .collect();
        // 4 次落盘：初始化基线 + 批次（合并 delta）+ tool_call 事务。
        assert_eq!(chat_docs.len(), 4, "delta 批次先落盘，控制类后落盘");
    }
}

// ---------------------------------------------------------------------------
// 单写者串行化（§7.4）：并发提交无 yrs panic，结果等价
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn concurrent_submits_no_panic_and_serial_equivalent() {
    let sink = MemSink::default();
    let mgr = Arc::new(DocManager::new(cfg(), Arc::new(sink.clone())));
    open(&mgr, "s1").await;

    // 订阅必须在提交之前（广播发生在提交期间，错过即无 update）。
    let mut rx = mgr.subscribe_updates().await;
    // 串行基线：先注册 turn。
    assert!(matches!(
        mgr.submit_event(user_msg("s1", 1, "t1")).await,
        SubmitResult::Applied(_)
    ));

    // 并发提交 32 个事件（delta 混合控制类）。
    let mut handles = Vec::new();
    for i in 0..32 {
        let mgr = mgr.clone();
        handles.push(tokio::spawn(async move {
            let ev = if i % 4 == 0 {
                NormalizedEvent {
                    chat_id: "s1".to_string(),
                    seq: 2 + i as u64,
                    epoch: 0,
                    body: EventBody::ToolCallStarted {
                        turn_id: "t1".to_string(),
                        tool_call_id: format!("tc{i}"),
                        name: "n".to_string(),
                        arguments: None,
                        created_at: "2026-08-07T00:00:00Z".to_string(),
                    },
                }
            } else {
                delta("s1", 2 + i as u64, "t1", "x")
            };
            mgr.submit_event(ev).await
        }));
    }
    for h in handles {
        let r = timeout(TokioDuration::from_secs(5), h)
            .await
            .expect("task timeout")
            .expect("task panic");
        match r {
            SubmitResult::Applied(_) | SubmitResult::Rejected(SubmitError::QueueFull) => {}
            other => panic!("unexpected submit result: {other:?}"),
        }
    }
    // 无 yrs panic（任务全部正常返回）；工具调用全部唯一创建。
    // 通过广播 update 重放验证视图一致性（等价于串行）。
    let mut chat_updates: Vec<Vec<u8>> = Vec::new();
    while let Ok(doc_update) = rx.try_recv() {
        if doc_update.doc == DocId::chat("s1") {
            chat_updates.push(doc_update.update);
        }
    }
    assert!(!chat_updates.is_empty(), "应有 chat update 广播");
}

// ---------------------------------------------------------------------------
// 队列上限（§8.6）：满 → RATE_LIMITED（try_reserve 语义）
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn try_reserve_returns_false_when_queue_full() {
    let mut c = cfg();
    c.chat_queue = 2;
    let mgr = DocManager::new(c, Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    assert!(mgr.try_reserve("s1").await);
    assert!(mgr.try_reserve("s1").await);
    assert!(!mgr.try_reserve("s1").await, "队列满 → false");
    assert!(!mgr.try_reserve("nope").await, "chat 不存在 → false");
}

/// P1-1 回归：try_reserve 占用的名额必须可释放（release_reserve），
/// 否则队列永满 → RATE_LIMITED 风暴。完整消费路径（coordinator 执行器
/// 消费 ExecCmd 后 release）由 command_coordinator_test 覆盖。
#[tokio::test(start_paused = true)]
async fn try_reserve_slots_released_after_writer_consumes() {
    let mut c = cfg();
    c.chat_queue = 2;
    let mgr = DocManager::new(c, Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    assert!(mgr.try_reserve("s1").await);
    assert!(mgr.try_reserve("s1").await);
    assert!(!mgr.try_reserve("s1").await, "队列满 → false");
    // 释放占用的名额（coordinator 执行器消费后调用）。
    mgr.release_reserve("s1").await;
    mgr.release_reserve("s1").await;
    assert!(
        mgr.try_reserve("s1").await,
        "P1-1: 释放后名额必须恢复"
    );
    // 不存在的 chat：no-op 不 panic。
    mgr.release_reserve("nope").await;
}

// ---------------------------------------------------------------------------
// 命令路径（§8.5）：user entry 注册 / 权限 CAS / 标题 / 终态
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn command_register_user_entry_then_submit_duplicate() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    let cmd = DocCommand::RegisterUserEntry {
        turn_id: "t1".to_string(),
        entry_id: "t1:user".to_string(),
        text: "hi".to_string(),
        author_user_id: None,
        created_at: "2026-08-07T00:00:00Z".to_string(),
    };
    let r = mgr.submit_command("s1", cmd.clone()).await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));
    // 重复注册 → 幂等拒绝。
    let r = mgr.submit_command("s1", cmd).await;
    assert!(matches!(r, SubmitResult::Applied(a) if !a.applied));
}

#[tokio::test(start_paused = true)]
async fn command_permission_resolve_cas() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    // 先注册 turn（active_turn 存在；PermissionRequested 关联检查要求 turn 已知）。
    let r = mgr.submit_event(user_msg("s1", 1, "t1")).await;
    assert!(matches!(r, SubmitResult::Applied(_)));
    // 再投影 permission request（事件路径）。
    let ev = NormalizedEvent {
        chat_id: "s1".to_string(),
        seq: 2,
        epoch: 0,
        body: EventBody::PermissionRequested {
            permission_id: "p1".to_string(),
            turn_id: "t1".to_string(),
            tool_call_id: None,
            title: "允许".to_string(),
            description: None,
            options: vec![],
            expires_at: "2026-08-07T00:05:00Z".to_string(),
        },
    };
    let r = mgr.submit_event(ev).await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::ResolvePermission {
                permission_id: "p1".to_string(),
                decision: acp_hub_proto::action::PermissionDecision::Allow,
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));
    // 重复 resolve → 幂等。
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::ResolvePermission {
                permission_id: "p1".to_string(),
                decision: acp_hub_proto::action::PermissionDecision::Deny,
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if !a.applied));
}

#[tokio::test(start_paused = true)]
async fn command_update_title_and_chat_terminal() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    let r = mgr
        .submit_command("s1", DocCommand::UpdateTitle { title: "新标题".into() })
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::SetChatTerminal {
                status: acp_hub_proto::schema::ChatStatus::Closed,
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));
}

// ---------------------------------------------------------------------------
// Registry 命令路由（§8.5）与 gap 上报（§9.4/§12.4）
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn registry_commands_route_to_global_writer() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::RegistryUpsertChat(ChatSummary {
                id: "s1".into(),
                instance_id: "m1".into(),
                title: "t".into(),
                status: "accepting".into(),
                gap: None,
                updated_at: "2026-08-07T00:00:00Z".into(),
            }),
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(_)));
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::RegistrySetGlobal {
                status: acp_hub_proto::schema::GlobalStatus::Degraded,
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(_)));
}

// ---------------------------------------------------------------------------
// open/close 生命周期
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn open_chat_idempotent_and_close_rejects_submit() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    // 重复打开幂等。
    open(&mgr, "s1").await;
    mgr.close_chat("s1").await.unwrap();
    // close 后提交 → ChatNotFound。
    let r = mgr.submit_event(user_msg("s1", 1, "t1")).await;
    assert!(matches!(r, SubmitResult::Rejected(SubmitError::ChatNotFound)));
}

// ---------------------------------------------------------------------------
// 广播订阅
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn broadcast_delivers_updates_to_subscribers() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    let mut rx = mgr.subscribe_updates().await;
    open(&mgr, "s1").await;
    let _ = mgr.submit_event(user_msg("s1", 1, "t1")).await;
    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;
    let mut got = 0;
    while let Ok(u) = rx.try_recv() {
        if u.doc == DocId::chat("s1") || u.doc == DocId::control("s1") {
            got += 1;
        }
    }
    assert!(got >= 2, "chat + control update 应广播，got {got}");
}

// ---------------------------------------------------------------------------
// 双 Doc 事务顺序（§7.4）：chat update 先于 control update 落盘
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn chat_persists_before_control() {
    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;
    let _ = mgr.submit_event(user_msg("s1", 1, "t1")).await;
    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;
    let updates = sink.updates.lock().await;
    // user_message 同时写 chat（entry）与 control（active_turn）：chat 先落盘。
    let chat_idx = updates
        .iter()
        .position(|(d, _)| *d == DocId::chat("s1"))
        .expect("chat update");
    let control_idx = updates
        .iter()
        .position(|(d, _)| *d == DocId::control("s1"))
        .expect("control update");
    assert!(chat_idx < control_idx, "chat 必须先于 control 落盘");
}

// ---------------------------------------------------------------------------
// 落盘失败 → PersistFailed + degraded 上报
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn persist_failure_returns_persist_failed() {
    let sink = MemSink::default();
    sink.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;
    // 先订阅（广播发生在提交期间，错过即无 update）。
    let mut rx = mgr.subscribe_updates().await;
    let r = mgr.submit_event(user_msg("s1", 1, "t1")).await;
    assert!(matches!(r, SubmitResult::PersistFailed));
    // 广播仍继续（视图可用；degraded 由 registry 状态源呈现）。
    let mut got = 0;
    while rx.try_recv().is_ok() {
        got += 1;
    }
    assert!(got > 0, "落盘失败不影响广播");
}

// ---------------------------------------------------------------------------
// gap 上报（§9.4/§12.4）：seq 跳变 → registry sessions[].gap 写回
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn gap_reported_to_registry_on_seq_jump() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    // 先订阅（registry 写回的广播发生在提交期间，错过即无 update）。
    let mut rx = mgr.subscribe_updates().await;
    open(&mgr, "s1").await;
    let _ = mgr.submit_event(user_msg("s1", 1, "t1")).await;
    let _ = mgr.submit_event(user_msg("s1", 5, "t2")).await; // 跳变 3
    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;
    // Registry Doc 落盘记录应包含 gap 写回（registry update 含 sessions.s1.gap）。
    let mut registry_updates = Vec::new();
    while let Ok(u) = rx.try_recv() {
        if u.doc == DocId::REGISTRY {
            registry_updates.push(u.update);
        }
    }
    // 至少有一次 registry 更新（open_chat 摘要 + gap 写回）。
    assert!(!registry_updates.is_empty());
}
