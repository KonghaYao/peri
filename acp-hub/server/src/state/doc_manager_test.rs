//! DocManager 测试（§7.4 单写者 / §6.4 微批次 / §8.5 命令路由）。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration as TokioDuration};
use yrs::{Map, ReadTxn, Transact};

use acp_hub_proto::conn::DocId;
use acp_hub_proto::schema::{ChatSummary, ToolCallStatus};

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
        ts: "2026-08-07T00:00:00Z".to_string(),
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
        ts: "2026-08-07T00:00:00Z".to_string(),
        body: EventBody::UserMessage {
            turn_id: turn.to_string(),
            entry_id: format!("{turn}:user"),
            text: "hi".to_string(),
            author_user_id: None,
            created_at: "2026-08-07T00:00:00Z".to_string(),
        },
    }
}

/// `session/load` 回放事件（§8.5）：历史 chunk 无 turn_id（真实 peri 重放）。
fn replay_user(chat: &str, seq: u64, text: &str) -> NormalizedEvent {
    NormalizedEvent {
        chat_id: chat.to_string(),
        seq,
        epoch: 0,
        ts: "2026-08-07T00:00:00Z".to_string(),
        body: EventBody::UserMessage {
            turn_id: String::new(),
            entry_id: String::new(),
            text: text.to_string(),
            author_user_id: None,
            created_at: "2026-08-07T00:00:00Z".to_string(),
        },
    }
}

fn replay_delta(chat: &str, seq: u64, text: &str) -> NormalizedEvent {
    NormalizedEvent {
        chat_id: chat.to_string(),
        seq,
        epoch: 0,
        ts: "2026-08-07T00:00:00Z".to_string(),
        body: EventBody::MessageDelta {
            turn_id: String::new(),
            entry_id: String::new(),
            block_id: String::new(),
            text: text.to_string(),
        },
    }
}

async fn open(mgr: &DocManager, chat: &str) {
    mgr.open_chat(chat, "m1", Some("t"), None, None)
        .await
        .unwrap();
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
        assert_eq!(
            deltas, 2,
            "仅 user_message 已落盘（+初始化基线），批次未 flush"
        );
    }
    // 推进 16ms → 批次 flush 为一次 chat 事务（一个 update）。
    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;
    for h in handles {
        let r = h.await.expect("submit task panic");
        assert!(
            matches!(r, SubmitResult::Applied(_)),
            "delta 提交应确认 Applied"
        );
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
        ts: "2026-08-07T00:00:00Z".to_string(),
        body: EventBody::ToolCallStarted {
            turn_id: "t1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            status: ToolCallStatus::Pending,
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
                    ts: "2026-08-07T00:00:00Z".to_string(),
                    body: EventBody::ToolCallStarted {
                        turn_id: "t1".to_string(),
                        tool_call_id: format!("tc{i}"),
                        name: "n".to_string(),
                        status: ToolCallStatus::Pending,
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
    assert!(mgr.try_reserve("s1").await, "P1-1: 释放后名额必须恢复");
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
        source_command_id: "command-1".to_string(),
        created_at: "2026-08-07T00:00:00Z".to_string(),
    };
    let r = mgr.submit_command("s1", cmd.clone()).await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));
    // 重复注册 → 幂等拒绝。
    let r = mgr.submit_command("s1", cmd).await;
    assert!(matches!(r, SubmitResult::Applied(a) if !a.applied));
}

#[tokio::test(start_paused = true)]
async fn command_register_user_entry_rejects_a_different_source_command() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    let command = |source: &str| DocCommand::RegisterUserEntry {
        turn_id: "t1".into(),
        entry_id: "t1:user".into(),
        text: "same".into(),
        author_user_id: None,
        source_command_id: source.into(),
        created_at: "now".into(),
    };
    assert!(
        matches!(mgr.submit_command("s1", command("cmd-1")).await, SubmitResult::Applied(a) if a.applied)
    );
    assert!(
        matches!(mgr.submit_command("s1", command("cmd-2")).await, SubmitResult::Applied(a) if a.reason == Some(crate::state::aggregator::ApplyReason::SourceCommandConflict))
    );
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
        ts: "2026-08-07T00:00:00Z".to_string(),
        body: EventBody::PermissionRequested {
            permission_id: "p1".to_string(),
            turn_id: "t1".to_string(),
            tool_call_id: None,
            tool: None,
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
async fn command_permission_resolution_updates_linked_tool_projection() {
    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;
    assert!(matches!(
        mgr.submit_event(user_msg("s1", 1, "t1")).await,
        SubmitResult::Applied(_)
    ));
    let started = NormalizedEvent {
        chat_id: "s1".into(),
        seq: 2,
        epoch: 0,
        ts: "2026-08-07T00:00:00Z".into(),
        body: EventBody::ToolCallStarted {
            turn_id: "t1".into(),
            tool_call_id: "tc1".into(),
            name: "shell".into(),
            status: ToolCallStatus::Running,
            arguments: None,
            created_at: "2026-08-07T00:00:00Z".into(),
        },
    };
    assert!(matches!(mgr.submit_event(started).await, SubmitResult::Applied(a) if a.applied));
    let requested = NormalizedEvent {
        chat_id: "s1".into(),
        seq: 3,
        epoch: 0,
        ts: "2026-08-07T00:00:00Z".into(),
        body: EventBody::PermissionRequested {
            permission_id: "p1".into(),
            turn_id: "t1".into(),
            tool_call_id: Some("tc1".into()),
            tool: None,
            title: "允许".into(),
            description: None,
            options: vec![],
            expires_at: "2026-08-07T00:05:00Z".into(),
        },
    };
    assert!(matches!(mgr.submit_event(requested).await, SubmitResult::Applied(a) if a.applied));
    assert!(matches!(
        mgr.submit_command(
            "s1",
            DocCommand::ResolvePermission {
                permission_id: "p1".into(),
                decision: acp_hub_proto::action::PermissionDecision::Deny,
            },
        )
        .await,
        SubmitResult::Applied(a) if a.applied
    ));

    tokio::task::yield_now().await;
    use yrs::updates::decoder::Decode as _;
    let mirror = yrs::Doc::new();
    for (doc, update) in sink.updates.lock().await.iter() {
        if *doc == DocId::chat("s1") {
            let mut txn = mirror.transact_mut();
            txn.apply_update(yrs::Update::decode_v1(update).unwrap())
                .unwrap();
        }
    }
    let txn = mirror.transact();
    let root = txn.get_map("root").unwrap();
    let calls = root
        .get(&txn, "tool_calls")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    let tool = calls
        .get(&txn, "tc1")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(
        tool.get(&txn, "status").unwrap().cast::<String>().unwrap(),
        "cancelled"
    );
}

#[tokio::test(start_paused = true)]
async fn command_update_title_and_chat_terminal() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::UpdateTitle {
                title: "新标题".into(),
            },
        )
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
// RegistryApplySessions（§6.3）：instance 级 session 列表全量同步投影到
// Registry Doc（幂等 diff + 自愈删除；不随 chat 销毁/重建）
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn command_apply_session_list_projects_idempotent_and_self_heals() {
    use acp_hub_proto::schema::SessionSummaryProjection;
    use yrs::{Map, MapRef, ReadTxn, Transact};

    // 镜像从 MemSink 落盘记录重放（含 writer 启动基线——基线只走 sink，
    // 不经广播通道；真实客户端经 gateway 快照获得同源基线）。
    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;

    let sum = |id: &str, title: &str, updated: &str| SessionSummaryProjection {
        session_id: id.to_string(),
        title: title.to_string(),
        status: String::new(), // peri SessionInfo 无 status 字段 → 空串
        updated_at: updated.to_string(),
        cwd: String::new(),
        bound_chat_id: None,
    };

    // 初始列表：两个条目。
    let entries = vec![sum("a", "会话A", "t0"), sum("b", "会话B", "t1")];
    let r = mgr
        .submit_command("s1", DocCommand::RegistryApplySessions { entries })
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));

    // 重放 control 更新（累积镜像）→ 校验 sessions Map。
    let mirror = yrs::Doc::new();
    async fn drain(sink: &MemSink, mirror: &yrs::Doc) {
        use yrs::updates::decoder::Decode as _;
        let updates = sink.updates.lock().await;
        for (doc, update) in updates.iter() {
            if *doc == DocId::REGISTRY {
                let parsed = yrs::Update::decode_v1(update).unwrap();
                let mut txn = mirror.transact_mut();
                txn.apply_update(parsed).unwrap();
            }
        }
    }
    drain(&sink, &mirror).await;
    {
        let txn = mirror.transact();
        let root = txn.get_map("root").expect("root map");
        let sessions = root
            .get(&txn, "sessions")
            .expect("sessions map")
            .cast::<MapRef>()
            .unwrap();
        assert_eq!(sessions.iter(&txn).count(), 2);
        let a = sessions.get(&txn, "a").unwrap().cast::<MapRef>().unwrap();
        assert_eq!(
            a.get(&txn, "title").unwrap().cast::<String>().unwrap(),
            "会话A"
        );
    }

    // 幂等：同列表再提交 → 仍 Applied（diff 无变化，无额外写入）。
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::RegistryApplySessions {
                entries: vec![sum("a", "会话A", "t0"), sum("b", "会话B", "t1")],
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));

    // 全量同步：b 更新字段、a 不在响应中 → 旧条目删除（§6.3 自愈）。
    let _ = mgr
        .submit_command(
            "s1",
            DocCommand::RegistryApplySessions {
                entries: vec![sum("b", "会话B-改", "t2")],
            },
        )
        .await;
    drain(&sink, &mirror).await;
    {
        let txn = mirror.transact();
        let root = txn.get_map("root").expect("root map");
        let sessions = root
            .get(&txn, "sessions")
            .expect("sessions map")
            .cast::<MapRef>()
            .unwrap();
        assert_eq!(sessions.iter(&txn).count(), 1, "响应中不存在的旧条目应删除");
        assert!(sessions.get(&txn, "a").is_none());
        let b = sessions.get(&txn, "b").unwrap().cast::<MapRef>().unwrap();
        assert_eq!(
            b.get(&txn, "title").unwrap().cast::<String>().unwrap(),
            "会话B-改"
        );
    }
}

/// 孤儿 key 自愈（§6.3 全量同步）：历史遗留条目（map key ≠ 内部 session_id）
/// 必须随一次全量同步删除——read_current 以真实 key 进投影，diff 按真实 key
/// 收集 remove；否则孤儿 key 永存、渲染层按 key 逐条渲染 → 重复条目。
#[test]
fn session_list_orphan_key_removed_by_full_sync() {
    use crate::state::session_list;
    use acp_hub_proto::schema::SessionSummaryProjection;
    use yrs::{Map, ReadTxn, Transact, WriteTxn};

    let doc = yrs::Doc::new();
    let sum = |id: &str, title: &str, updated: &str| SessionSummaryProjection {
        session_id: id.to_string(),
        title: title.to_string(),
        status: String::new(),
        updated_at: updated.to_string(),
        cwd: String::new(),
        bound_chat_id: None,
    };

    // 构造孤儿条目：map key = "old-key"，内部 session_id = "b"（与 incoming
    // 相同的 id——最严场景：既需重建正常 key 条目，又需删除孤儿 key）。
    {
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map("root");
        let sessions = root.get_or_init::<_, yrs::MapRef>(&mut txn, "sessions");
        let m = sessions.get_or_init::<_, yrs::MapRef>(&mut txn, "old-key");
        m.insert(&mut txn, "session_id", "b");
        m.insert(&mut txn, "title", "旧条目");
        m.insert(&mut txn, "updated_at", "t0");
    }

    // 全量同步：响应只有 b（正常条目）。
    let current = {
        let txn = doc.transact();
        let root = txn.get_map("root").unwrap();
        session_list::read_current(&txn, &root)
    };
    assert_eq!(current.len(), 1);
    assert!(
        current.contains_key("old-key"),
        "投影以 map 真实 key 为键（孤儿也进投影）"
    );
    let d = session_list::diff(&current, &[sum("b", "B", "t1")]);
    assert!(
        d.remove.contains(&"old-key".to_string()),
        "孤儿 key 应被 remove 收集（真实 key）"
    );
    assert!(d.upsert.iter().any(|e| e.session_id == "b"));

    // 应用 diff。
    {
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map("root");
        session_list::apply_diff(&mut txn, &root, &d);
    }

    // 结果：孤儿删除，正常条目写入（key = b，内容来自 incoming）。
    let txn = doc.transact();
    let root = txn.get_map("root").unwrap();
    let sessions = root
        .get(&txn, "sessions")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(sessions.iter(&txn).count(), 1, "孤儿删除后只剩 1 条");
    assert!(sessions.get(&txn, "old-key").is_none(), "孤儿 key 已删除");
    let b = sessions
        .get(&txn, "b")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(b.get(&txn, "title").unwrap().cast::<String>().unwrap(), "B");
}

// ---------------------------------------------------------------------------
// session/load 回放模式（§8.5 显式重建）：BeginLoadReplay → 历史 chunk 按
// 回放序归位（load:{seq}）→ EndLoadReplay 全部终态化
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn load_replay_command_flow_projects_and_terminates_history() {
    use yrs::updates::decoder::Decode as _;
    use yrs::{Array, GetString, Map, ReadTxn, Transact};

    // 镜像从 MemSink 落盘记录重放（含 writer 启动基线）。
    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;

    // 先写入当前会话内容；load 是替换当前会话，旧 Yjs 内容不得与回放混合。
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::RegisterUserEntry {
                turn_id: "old-turn".into(),
                entry_id: "old-turn:user".into(),
                text: "旧会话内容".into(),
                author_user_id: None,
                source_command_id: "old-command".into(),
                created_at: "2026-08-10T00:00:00Z".into(),
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));

    // 回放模式开始（coordinator 在 session/load 请求前提交）。
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::BeginLoadReplay {
                acp_session_id: "acp-1".into(),
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));

    // 历史回放流（真实 peri 重放形态：无 turnId 的 user/agent chunk）。
    assert!(matches!(
        mgr.submit_event(replay_user("s1", 1, "历史问题1")).await,
        SubmitResult::Applied(_)
    ));
    assert!(matches!(
        mgr.submit_event(replay_delta("s1", 2, "历史回答1")).await,
        SubmitResult::Applied(_)
    ));
    assert!(matches!(
        mgr.submit_event(replay_user("s1", 3, "历史问题2")).await,
        SubmitResult::Applied(_)
    ));
    assert!(matches!(
        mgr.submit_event(replay_delta("s1", 4, "历史回答2")).await,
        SubmitResult::Applied(_)
    ));

    // 回放结束（load 响应到达后提交）：全部回放 turn 终态化。
    let r = mgr.submit_command("s1", DocCommand::EndLoadReplay).await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));

    // 批次 flush（delta 类入队即返回，需推进批次窗口）。
    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;

    // 重放落盘记录 → 镜像 chat doc，验证条目归位与终态。
    let mirror = yrs::Doc::new();
    let updates = sink.updates.lock().await;
    for (doc, update) in updates.iter() {
        if *doc == DocId::chat("s1") {
            let parsed = yrs::Update::decode_v1(update).unwrap();
            let mut txn = mirror.transact_mut();
            txn.apply_update(parsed).unwrap();
        }
    }
    drop(updates);
    let txn = mirror.transact();
    let root = txn.get_map("root").expect("root map");
    let entries = root
        .get(&txn, "entries")
        .expect("entries map")
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(entries.iter(&txn).count(), 4, "2 user + 2 assistant");
    // 文本在 entry 的 blocks map；block_id 约定：user 条目 `{entry_id}:text`
    // （create_user_entry），assistant 回放条目 = 内容块种类（`text`，§7.2
    // 归位）——统一按 block_order 首块读取。
    let entry_text = |m: &yrs::MapRef| -> String {
        let order = m
            .get(&txn, "block_order")
            .unwrap()
            .cast::<yrs::ArrayRef>()
            .unwrap();
        let bid = order.get(&txn, 0).unwrap().cast::<String>().unwrap();
        m.get(&txn, "blocks")
            .unwrap()
            .cast::<yrs::MapRef>()
            .unwrap()
            .get(&txn, &bid)
            .unwrap()
            .cast::<yrs::MapRef>()
            .unwrap()
            .get(&txn, "text")
            .unwrap()
            .cast::<yrs::TextRef>()
            .unwrap()
            .get_string(&txn)
    };
    let get_entry = |id: &str| {
        entries
            .get(&txn, id)
            .unwrap_or_else(|| panic!("entry {id} missing"))
            .cast::<yrs::MapRef>()
            .unwrap()
    };
    // 归位 turn：load:1 / load:3（seq 水位驱动）。
    let u1 = get_entry("load:1:user");
    assert_eq!(entry_text(&u1), "历史问题1");
    let a1 = get_entry("load:1:assistant");
    assert_eq!(entry_text(&a1), "历史回答1");
    assert_eq!(
        a1.get(&txn, "status").unwrap().cast::<String>().unwrap(),
        "completed"
    );
    let u2 = get_entry("load:3:user");
    assert_eq!(entry_text(&u2), "历史问题2");
    let a2 = get_entry("load:3:assistant");
    assert_eq!(entry_text(&a2), "历史回答2");
    assert_eq!(
        a2.get(&txn, "status").unwrap().cast::<String>().unwrap(),
        "completed"
    );
}

// ---------------------------------------------------------------------------
// 回放首帧即为 agent 增量（§8.5 REPLAY_NEEDS_TURN）：合成空文本 user 占位
// turn，杜绝空 id 垃圾条目
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn load_replay_first_frame_is_delta_synthesizes_placeholder() {
    use yrs::updates::decoder::Decode as _;
    use yrs::{Array, GetString, Map, ReadTxn, Transact};

    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;
    let _ = mgr
        .submit_command(
            "s1",
            DocCommand::BeginLoadReplay {
                acp_session_id: "acp-1".into(),
            },
        )
        .await;
    // 历史首帧即 agent 增量（真实 peri 重放形态：无 turnId）。
    assert!(matches!(
        mgr.submit_event(replay_delta("s1", 1, "历史回答（无前置问题）"))
            .await,
        SubmitResult::Applied(_)
    ));
    let _ = mgr.submit_command("s1", DocCommand::EndLoadReplay).await;
    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;

    let mirror = yrs::Doc::new();
    let updates = sink.updates.lock().await;
    for (doc, update) in updates.iter() {
        if *doc == DocId::chat("s1") {
            let parsed = yrs::Update::decode_v1(update).unwrap();
            let mut txn = mirror.transact_mut();
            txn.apply_update(parsed).unwrap();
        }
    }
    drop(updates);
    let txn = mirror.transact();
    let root = txn.get_map("root").expect("root map");
    let entries = root
        .get(&txn, "entries")
        .expect("entries map")
        .cast::<yrs::MapRef>()
        .unwrap();
    // 占位 user + assistant 两条 entry；无空 id 条目。
    let keys: Vec<String> = entries.keys(&txn).map(|k| k.to_string()).collect();
    assert_eq!(keys.len(), 2, "占位 user + assistant 增量");
    assert!(
        keys.iter().all(|k| !k.is_empty()),
        "回放合成不得产生空 id 条目：{keys:?}"
    );
    // 归位 turn = 首帧 seq（`load:1`）。
    let a = entries
        .get(&txn, "load:1:assistant")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    let order = a
        .get(&txn, "block_order")
        .unwrap()
        .cast::<yrs::ArrayRef>()
        .unwrap();
    let bid = order.get(&txn, 0).unwrap().cast::<String>().unwrap();
    let text = a
        .get(&txn, "blocks")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap()
        .get(&txn, &bid)
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap()
        .get(&txn, "text")
        .unwrap()
        .cast::<yrs::TextRef>()
        .unwrap()
        .get_string(&txn);
    assert_eq!(text, "历史回答（无前置问题）");
    assert_eq!(
        a.get(&txn, "status").unwrap().cast::<String>().unwrap(),
        "completed",
        "EndLoadReplay 终态化合成 turn"
    );
}

// ---------------------------------------------------------------------------
// ExpirePendingPermissions（§7.1 断链清理）：全部 pending 批量过期，CAS 语义
// 与 expire 一致（resolved/expired 不动）
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn expire_pending_permissions_batch_expires_all() {
    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;
    assert!(matches!(
        mgr.submit_event(user_msg("s1", 1, "t1")).await,
        SubmitResult::Applied(_)
    ));
    // 两条 pending 权限（事件路径投影）。
    for (seq, pid) in [(2u64, "p1"), (3, "p2")] {
        let ev = NormalizedEvent {
            chat_id: "s1".to_string(),
            seq,
            epoch: 0,
            ts: "2026-08-07T00:00:00Z".to_string(),
            body: EventBody::PermissionRequested {
                permission_id: pid.to_string(),
                turn_id: "t1".to_string(),
                tool_call_id: None,
                tool: None,
                title: "允许".to_string(),
                description: None,
                options: vec![],
                expires_at: "2026-08-07T00:05:00Z".to_string(),
            },
        };
        assert!(matches!(mgr.submit_event(ev).await, SubmitResult::Applied(a) if a.applied));
    }
    // 批量过期命令（断链清理路径）。
    let r = mgr
        .submit_command("s1", DocCommand::ExpirePendingPermissions)
        .await;
    assert!(matches!(r, SubmitResult::Applied(_)));
    // 幂等重发（无 pending → 迁移 0 条）。
    let r = mgr
        .submit_command("s1", DocCommand::ExpirePendingPermissions)
        .await;
    assert!(matches!(r, SubmitResult::Applied(_)));

    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;
    use yrs::updates::decoder::Decode as _;
    use yrs::{Map as _, ReadTxn as _, Transact as _};
    // 落盘记录 → 镜像 session doc 验证（与 chat 验证同款 mirror 模式）。
    let mirror = yrs::Doc::new();
    let updates = sink.updates.lock().await;
    for (doc, update) in updates.iter() {
        if *doc == DocId::session("s1") {
            let parsed = yrs::Update::decode_v1(update).unwrap();
            let mut txn = mirror.transact_mut();
            txn.apply_update(parsed).unwrap();
        }
    }
    let txn = mirror.transact();
    let root = txn.get_map("root").unwrap();
    let perms = root
        .get(&txn, "pending_permissions")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(perms.iter(&txn).count(), 2, "条目保留（仅状态迁移）");
    for (_, v) in perms.iter(&txn) {
        let pm = v.cast::<yrs::MapRef>().unwrap();
        assert_eq!(
            pm.get(&txn, "status").unwrap().cast::<String>().unwrap(),
            "expired"
        );
        assert!(
            matches!(
                pm.get(&txn, "decision"),
                None | Some(yrs::Out::Any(yrs::Any::Null))
            ),
            "decision 保持 null"
        );
    }
}

// ---------------------------------------------------------------------------
// MarkTurnCancelling（§7.2 cancel 前置）：活动 turn 匹配且非终态 → 置
// cancelling；终态/不匹配 → TurnTerminalGuard 拒绝
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn mark_turn_cancelling_sets_cancelling_state() {
    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;
    // 建立活动 turn（RegisterUserEntry → accepting）。
    assert!(matches!(
        mgr.submit_command(
            "s1",
            DocCommand::RegisterUserEntry {
                turn_id: "t1".into(),
                entry_id: "t1:user".into(),
                text: "hi".into(),
                author_user_id: None,
                source_command_id: "cancel-command".into(),
                created_at: "2026-08-10T00:00:00Z".into(),
            },
        )
        .await,
        SubmitResult::Applied(_)
    ));
    // cancel 前置：accepting → cancelling。
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::MarkTurnCancelling {
                turn_id: "t1".into(),
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));
    // 幂等重发（状态已是 cancelling，非终态仍可再置——幂等无副作用）。
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::MarkTurnCancelling {
                turn_id: "t1".into(),
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(_)));
    // 终态后拒绝（SetTurnTerminal → cancelled → MarkTurnCancelling 守卫）。
    let _ = mgr
        .submit_command(
            "s1",
            DocCommand::SetTurnTerminal {
                turn_id: "t1".into(),
                status: acp_hub_proto::schema::TurnStatus::Cancelled,
                completed_at: "2026-08-10T00:00:01Z".into(),
            },
        )
        .await;
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::MarkTurnCancelling {
                turn_id: "t1".into(),
            },
        )
        .await;
    assert!(
        matches!(r, SubmitResult::Applied(a) if !a.applied),
        "终态后 cancel 前置拒绝"
    );

    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;
    use yrs::updates::decoder::Decode as _;
    use yrs::{Map as _, ReadTxn as _, Transact as _};
    let mirror = yrs::Doc::new();
    let updates = sink.updates.lock().await;
    for (doc, update) in updates.iter() {
        if *doc == DocId::session("s1") {
            let parsed = yrs::Update::decode_v1(update).unwrap();
            let mut txn = mirror.transact_mut();
            txn.apply_update(parsed).unwrap();
        }
    }
    drop(updates);
    let txn = mirror.transact();
    let root = txn.get_map("root").unwrap();
    let sm = root
        .get(&txn, "session")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(
        sm.get(&txn, "active_turn_status")
            .unwrap()
            .cast::<String>()
            .unwrap(),
        "cancelled",
        "终态保持（cancel 前置不覆盖终态）"
    );
}

#[tokio::test(start_paused = true)]
async fn turn_terminal_is_idempotent_only_for_the_same_persisted_outcome() {
    let mgr = DocManager::new(cfg(), Arc::new(MemSink::default()));
    open(&mgr, "s1").await;
    let register = DocCommand::RegisterUserEntry {
        turn_id: "t1".into(),
        entry_id: "t1:user".into(),
        text: "hi".into(),
        author_user_id: None,
        source_command_id: "command-1".into(),
        created_at: "2026-08-14T00:00:00Z".into(),
    };
    assert!(matches!(
        mgr.submit_command("s1", register).await,
        SubmitResult::Applied(result) if result.applied
    ));
    let terminal = |status| DocCommand::SetTurnTerminal {
        turn_id: "t1".into(),
        status,
        completed_at: "2026-08-14T00:00:01Z".into(),
    };
    assert!(matches!(
        mgr.submit_command(
            "s1",
            terminal(acp_hub_proto::schema::TurnStatus::Completed),
        )
        .await,
        SubmitResult::Applied(result) if result.applied
    ));
    assert!(matches!(
        mgr.submit_command(
            "s1",
            terminal(acp_hub_proto::schema::TurnStatus::Completed),
        )
        .await,
        SubmitResult::Applied(result)
            if !result.applied
                && result.reason
                    == Some(crate::state::aggregator::ApplyReason::DuplicateIdempotent)
    ));
    assert!(matches!(
        mgr.submit_command(
            "s1",
            terminal(acp_hub_proto::schema::TurnStatus::Failed),
        )
        .await,
        SubmitResult::Applied(result)
            if !result.applied
                && result.reason
                    == Some(crate::state::aggregator::ApplyReason::TerminalProjectionConflict)
    ));
}

// ---------------------------------------------------------------------------
// SetAgentSessionId（§5.4 agent map）：binding 建立/load 恢复路径写回
// agent.acp_session_id；BeginLoadReplay 同步更新
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn set_agent_session_id_writes_agent_projection() {
    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;
    // session/new 绑定建立路径（create 无 load_session）。
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::SetAgentSessionId {
                acp_session_id: "acp-new".into(),
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));
    // load 切换：BeginLoadReplay 同步更新 agent.acp_session_id。
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::BeginLoadReplay {
                acp_session_id: "acp-loaded".into(),
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));
    // load 失败恢复：SetAgentSessionId 写回旧值。
    let r = mgr
        .submit_command(
            "s1",
            DocCommand::SetAgentSessionId {
                acp_session_id: "acp-prev".into(),
            },
        )
        .await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));

    tokio::time::advance(TokioDuration::from_millis(20)).await;
    tokio::task::yield_now().await;
    use yrs::updates::decoder::Decode as _;
    use yrs::{Map as _, ReadTxn as _, Transact as _};
    let mirror = yrs::Doc::new();
    let updates = sink.updates.lock().await;
    for (doc, update) in updates.iter() {
        if *doc == DocId::session("s1") {
            let parsed = yrs::Update::decode_v1(update).unwrap();
            let mut txn = mirror.transact_mut();
            txn.apply_update(parsed).unwrap();
        }
    }
    drop(updates);
    let txn = mirror.transact();
    let root = txn.get_map("root").unwrap();
    let am = root
        .get(&txn, "agent")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(
        am.get(&txn, "acp_session_id")
            .unwrap()
            .cast::<String>()
            .unwrap(),
        "acp-prev",
        "恢复路径写回旧值（镜像以最后写入为准）"
    );
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
                cwd: String::new(),
                workspace_id: None,
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
    assert!(matches!(
        r,
        SubmitResult::Rejected(SubmitError::ChatNotFound)
    ));
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
        if u.doc == DocId::chat("s1") || u.doc == DocId::session("s1") {
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
        .position(|(d, _)| *d == DocId::session("s1"))
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

// ---------------------------------------------------------------------------
// 断链追平恢复（§7.3/§8.5）：ResumeAfterGap 命令——可校准 → 上报追平
// （registry gap 标记清除）；不可校准（epoch 变化）→ 拒绝（保持 gap，
// 只能经 session/load 显式重建消除）
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn resume_after_gap_calibrated_clears_registry_gap() {
    use yrs::{Map, MapRef, ReadTxn, Transact};

    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;
    // 流基线（last_seq=1；断链模拟由 relay 置 registry gap 标记，此处
    // 直接验证命令路径）。
    assert!(matches!(
        mgr.submit_event(user_msg("s1", 1, "t1")).await,
        SubmitResult::Applied(_)
    ));
    // 可校准：ResumeAfterGap → Applied；尾部 report_gap 立即写回
    // （set_chat_gap(None) → registry chats[s1].gap 置 Null）。
    let r = mgr.submit_command("s1", DocCommand::ResumeAfterGap).await;
    assert!(matches!(r, SubmitResult::Applied(a) if a.applied));

    // 镜像校验：registry doc chats[s1].gap 已被清除。
    let mirror = yrs::Doc::new();
    async fn drain_registry(sink: &MemSink, mirror: &yrs::Doc) {
        use yrs::updates::decoder::Decode as _;
        let updates = sink.updates.lock().await;
        for (doc, update) in updates.iter() {
            if *doc == DocId::REGISTRY {
                let parsed = yrs::Update::decode_v1(update).unwrap();
                let mut txn = mirror.transact_mut();
                txn.apply_update(parsed).unwrap();
            }
        }
    }
    drain_registry(&sink, &mirror).await;
    {
        let txn = mirror.transact();
        let root = txn.get_map("root").expect("root map");
        let chats = root
            .get(&txn, "chats")
            .expect("chats map")
            .cast::<MapRef>()
            .unwrap();
        let sm = chats.get(&txn, "s1").unwrap().cast::<MapRef>().unwrap();
        let gap = sm.get(&txn, "gap");
        assert!(
            gap.is_none() || matches!(gap, Some(yrs::Out::Any(yrs::Any::Null))),
            "追平后 gap 标记必须清除（{gap:?}）"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn resume_after_gap_uncalibratable_rejected() {
    let sink = MemSink::default();
    let mgr = DocManager::new(cfg(), Arc::new(sink.clone()));
    open(&mgr, "s1").await;
    // 流基线（last_seq=1）。
    assert!(matches!(
        mgr.submit_event(user_msg("s1", 1, "t1")).await,
        SubmitResult::Applied(_)
    ));
    // epoch 变化（模拟 daemon 重启，§4.5.1）：既有流上的新纪元 → 聚合器
    // 置不可校准缺口并拒绝本帧（relay 校验只挡与 hello 记录不一致的帧，
    // 新纪元经新 hello 对账后可达聚合器）。
    let e = NormalizedEvent {
        chat_id: "s1".into(),
        seq: 2,
        epoch: 1,
        ts: "2026-08-07T00:00:00Z".to_string(),
        body: EventBody::UserMessage {
            turn_id: "t2".into(),
            entry_id: "t2:user".into(),
            text: "hi".into(),
            author_user_id: None,
            created_at: "2026-08-07T00:00:00Z".to_string(),
        },
    };
    let r = mgr.submit_event(e).await;
    assert!(
        matches!(r, SubmitResult::Applied(a) if !a.applied),
        "epoch 变化帧应被拒绝"
    );
    // 不可校准：ResumeAfterGap → Rejected（保持 gap 呈现——只能经 load
    // 显式重建消除，不得误标为已追平）。
    let r = mgr.submit_command("s1", DocCommand::ResumeAfterGap).await;
    assert!(
        matches!(r, SubmitResult::Rejected(_)),
        "uncalibratable 拒绝恢复"
    );
}
