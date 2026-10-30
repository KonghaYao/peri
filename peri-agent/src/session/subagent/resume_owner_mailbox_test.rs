use super::*;
use crate::agent::async_tasks::delivery::terminal_delivery_id;
use crate::agent::events::BackgroundTaskResult;
use crate::session::async_result_mailbox::AsyncResultState;
use crate::session::subagent::ParkedChildOwner;
use crate::session::{FrozenContext, MessageQueue, Session};
use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const RESULT_A: &str = "RESULT_A_UNCOVERED_FACT";
const RESULT_B: &str = "RESULT_B_LATE_FACT";

fn shell_result(id: &str, output: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: id.into(),
        agent_name: "shell".into(),
        prompt_summary: "shell".into(),
        success: true,
        output: output.into(),
        tool_calls_count: 0,
        duration_ms: 0,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

fn register_shell(
    manager: &TaskManager,
    id: &str,
) -> peri_acp_types::tasks::TaskRegistrationTicket {
    let ticket = manager
        .register(BgTaskRegistration {
            task_id: id.into(),
            kind: BgTaskKind::Shell,
            summary: "测试后台任务".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    // The synthetic task has no process; mailbox/result responsibility remains.
    manager.confirm_external_execution_stopped(&ticket);
    ticket
}

struct SummaryCapture(Arc<parking_lot::Mutex<Vec<peri_model::ModelRequest>>>);

#[async_trait::async_trait]
impl peri_model::Model for SummaryCapture {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        Err(peri_model::ModelError::cancelled())
    }

    async fn complete(
        &self,
        request: peri_model::ModelRequest,
        _cancellation: CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelResponse> {
        self.0.lock().push(request);
        peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text("<summary>已压缩历史</summary>"),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )
    }
}

#[derive(Clone)]
struct ResumeModel {
    messages: Arc<parking_lot::Mutex<Vec<Vec<BaseMessage>>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    first: Arc<AtomicBool>,
}

impl ResumeModel {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        _cancellation: CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        self.messages
            .lock()
            .push(crate::session::test_resources::mock::model::base_messages(
                &request,
            ));
        if self.first.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        crate::session::test_resources::mock::model::text_events("已处理结果")
    }
}
crate::fixture_model_impl!(ResumeModel);

type CapturedSession = (
    std::sync::Weak<Session>,
    Arc<crate::session::async_result_mailbox::AsyncResultMailbox>,
    peri_acp_types::tasks::OnBgCompleteFn,
);

struct CaptureSessionAssembler(Arc<parking_lot::Mutex<Option<CapturedSession>>>);

impl SubagentChainAssembler for CaptureSessionAssembler {
    fn assemble(&self, _ctx: &SubagentChainContext) -> MiddlewareChain {
        MiddlewareChain::new()
    }

    fn bind_tools(
        &self,
        session: &Arc<Session>,
        tools: Vec<Arc<dyn crate::tools::BaseTool>>,
        _filter: crate::session::tool_catalog::ToolFilter,
    ) -> Vec<Arc<dyn crate::tools::BaseTool>> {
        *self.0.lock() = Some((
            Arc::downgrade(session),
            session.async_result_mailbox(),
            session
                .subagent_host()
                .unwrap()
                .on_bg_complete
                .clone()
                .unwrap(),
        ));
        tools
    }
}

async fn failed_child_resume_preserves_blocked_result_through_full_compact(
    first_background: bool,
    background: bool,
) {
    let store = MockSessionResources::new();
    let child_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = child_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&child_id, "error")
        .await
        .unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let child_manager = Arc::new(TaskManager::new());
    let ticket_a = register_shell(&child_manager, "task-A");
    let ticket_b = register_shell(&child_manager, "task-B");
    let owner = ParkedChildOwner::new(
        child_id.clone(),
        "parent-thread".into(),
        child_manager.clone(),
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    parent_manager.register_child_owner(owner.clone()).unwrap();
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager.clone()),
        ..Default::default()
    });
    let initial_delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::session::bg_complete::queue_terminal_delivery(owner.queue.clone()),
    );
    child_manager
        .settle_completed(
            &ticket_a,
            shell_result("task-A", RESULT_A),
            initial_delivery,
        )
        .unwrap();
    let result_a_id = terminal_delivery_id("task-A", "terminal");
    let mut failed_config = resume_config_with(
        store.clone(),
        child_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(FailWithoutResidual),
            "fixture-scripted",
        ),
        if first_background {
            SubagentRunMode::Background
        } else {
            SubagentRunMode::Sync
        },
        Some(parent_manager.clone()),
        None,
    );
    let captured = Arc::new(parking_lot::Mutex::new(None));
    failed_config.chain_assembler = Arc::new(CaptureSessionAssembler(captured.clone()));
    let (failed_tx, failed_rx) = tokio::sync::oneshot::channel();
    let failed_tx = parking_lot::Mutex::new(Some(failed_tx));
    failed_config.on_bg_complete = Some(Arc::new(move |result, _| {
        if let Some(sender) = failed_tx.lock().take() {
            let _ = sender.send(result.clone());
        }
        Ok(())
    }));
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let finished_tx = parking_lot::Mutex::new(Some(finished_tx));
    failed_config.deregister_runtime = Some(Arc::new(move |_| {
        if let Some(sender) = finished_tx.lock().take() {
            let _ = sender.send(());
        }
    }));
    let failed = SessionFactory::resume_subagent(Some(&parent), failed_config).await;
    if first_background {
        let spawned = failed.expect("首次后台执行必须启动");
        let result = tokio::time::timeout(Duration::from_secs(10), failed_rx)
            .await
            .expect("首次失败必须交付")
            .unwrap();
        assert!(!result.success);
        drop(spawned);
    } else {
        let error = failed.err().expect("首次同步执行必须报告模型失败");
        assert!(error
            .downcast_ref::<crate::session::subagent::SubagentFailure>()
            .unwrap()
            .handoff()
            .is_some());
    }
    let (old_session, old_mailbox, old_delivery) =
        captured.lock().take().expect("必须捕获原会话的资源路由");
    tokio::time::timeout(Duration::from_secs(10), finished_rx)
        .await
        .expect("首次执行必须排空并结算")
        .unwrap();
    let blocked = old_mailbox.protected_snapshot();
    assert!(
        blocked.items.iter().any(|item| {
            item.delivery_id == result_a_id && item.state == AsyncResultState::Blocked
        }),
        "任务 A 必须先经 Receive 提交，再由模型失败标记 Blocked"
    );
    assert!(store
        .payloads_of(&child_id)
        .iter()
        .any(|payload| payload.id() == result_a_id));
    assert_eq!(child_manager.active_count(), 1, "任务 B 必须仍在运行");
    assert!(owner.receipt().is_some(), "任务 B 必须可靠交接");
    tokio::time::timeout(Duration::from_secs(10), async {
        while old_session.upgrade().is_some() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("资源 owner 不得保留旧计算 session");

    let summaries = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let messages = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let model = ResumeModel {
        messages: messages.clone(),
        entered: entered.clone(),
        release: release.clone(),
        first: Arc::new(AtomicBool::new(true)),
    };
    let mut resumed_config = resume_config_with(
        store.clone(),
        child_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(model),
            "fixture-scripted",
        ),
        if background {
            SubagentRunMode::Background
        } else {
            SubagentRunMode::Sync
        },
        Some(parent_manager),
        None,
    );
    resumed_config.compact_config = Some(crate::agent::CompactConfig {
        max_resident_history_bytes: 1,
        ..Default::default()
    });
    resumed_config.compact_llm = Some(Arc::new(SummaryCapture(summaries.clone())));
    let resumed_state = Arc::new(parking_lot::Mutex::new(None));
    let state_at_start = resumed_state.clone();
    let mailbox_at_start = old_mailbox.clone();
    resumed_config.on_subagent_start = Some(Arc::new(move |_, _, _| {
        *state_at_start.lock() = mailbox_at_start.state_of(&result_a_id);
    }));
    let (resumed_tx, resumed_rx) = tokio::sync::oneshot::channel();
    let resumed_tx = parking_lot::Mutex::new(Some(resumed_tx));
    resumed_config.on_bg_complete = Some(Arc::new(move |result, _| {
        if let Some(sender) = resumed_tx.lock().take() {
            let _ = sender.send(result.clone());
        }
        Ok(())
    }));
    let resumed_call = tokio::spawn(async move {
        SessionFactory::resume_subagent(Some(&parent), resumed_config).await
    });
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .expect("新执行必须进入主模型");
    let summary_contains_result = {
        let requests = summaries.lock();
        assert!(!requests.is_empty(), "新执行必须在 Reason 前实际执行 Full");
        serde_json::to_string(&requests[0])
            .unwrap()
            .contains(RESULT_A)
    };
    let excluded = store.flags(&result_a_id).excluded;
    let reason_contains_result = messages.lock()[0]
        .iter()
        .any(|message| message.content().contains(RESULT_A));
    assert!(
        !summary_contains_result && !excluded && reason_contains_result,
        "未覆盖结果必须受 Full 保护：summary_contains={summary_contains_result}, excluded={excluded}, reason_contains={reason_contains_result}"
    );
    assert_eq!(
        *resumed_state.lock(),
        Some(AsyncResultState::Ready),
        "显式 resume 开始执行时必须解除 Blocked"
    );
    assert_eq!(
        child_manager
            .registry()
            .list_tasks_full()
            .iter()
            .filter(|task| task.task_id == "task-B")
            .count(),
        1,
        "resume 不得重复注册任务"
    );
    let result_b = shell_result("task-B", RESULT_B);
    child_manager
        .settle_completed(&ticket_b, result_b.clone(), old_delivery.clone())
        .unwrap();
    old_delivery(&result_b, BgTaskKind::Shell).unwrap();
    let result_b_id = terminal_delivery_id("task-B", "terminal");
    assert_eq!(
        owner
            .queue
            .snapshot()
            .iter()
            .filter(|message| message.delivery_id == Some(result_b_id))
            .count(),
        1,
        "重复交付不得再次入队"
    );
    assert_eq!(child_manager.active_count(), 0);
    release.notify_one();
    let resumed = tokio::time::timeout(Duration::from_secs(10), resumed_call)
        .await
        .expect("恢复调用必须结束")
        .unwrap()
        .expect("可靠交接后必须允许 resume");
    assert_eq!(resumed.child_thread_id, child_id);
    assert!(Arc::ptr_eq(
        &old_mailbox,
        &resumed.session.async_result_mailbox()
    ));
    if background {
        let resumed_result = tokio::time::timeout(Duration::from_secs(10), resumed_rx)
            .await
            .expect("恢复执行必须处理 B 后完成")
            .unwrap();
        assert!(resumed_result.success);
    } else {
        assert!(resumed.task_id.is_none(), "同步恢复应直接完成");
    }
    let seen = messages.lock();
    assert!(seen
        .iter()
        .flatten()
        .any(|message| message.content().contains(RESULT_B)));
    assert!(
        old_mailbox.protected_snapshot().is_empty(),
        "A、B 均须由有效响应覆盖"
    );
    assert!(!old_mailbox.covered_is_empty());
}

/// [回归测试] 模型失败交接后，后台 resume 的 Full 保留未覆盖结果。
#[tokio::test(start_paused = true)]
async fn test_failed_child_background_resume_preserves_blocked_result_through_full_compact() {
    failed_child_resume_preserves_blocked_result_through_full_compact(true, true).await;
}

/// [回归测试] 后台失败交接后，同步 resume 的 Full 保留未覆盖结果。
#[tokio::test(start_paused = true)]
async fn test_failed_child_sync_resume_preserves_blocked_result_through_full_compact() {
    failed_child_resume_preserves_blocked_result_through_full_compact(true, false).await;
}

/// [回归测试] 同步失败交接后，后台 resume 的 Full 保留未覆盖结果。
#[tokio::test(start_paused = true)]
async fn test_sync_failed_child_background_resume_preserves_blocked_result_through_full_compact() {
    failed_child_resume_preserves_blocked_result_through_full_compact(false, true).await;
}

/// [回归测试] 同步失败交接后，同步 resume 的 Full 保留未覆盖结果。
#[tokio::test(start_paused = true)]
async fn test_sync_failed_child_sync_resume_preserves_blocked_result_through_full_compact() {
    failed_child_resume_preserves_blocked_result_through_full_compact(false, false).await;
}

/// [回归测试] 没有进程内 owner 的冷恢复只加载历史，不重建任务或结果责任。
#[tokio::test]
async fn test_cold_resume_does_not_reconstruct_mailbox_or_tasks() {
    use peri_acp_types::session_resources::SessionResources as _;

    let store = MockSessionResources::new();
    let child_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = child_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&child_id, "error")
        .await
        .unwrap();
    let id = terminal_delivery_id("old-task", "terminal");
    let reminder = crate::session::async_router::background_result_reminder(
        &shell_result("old-task", RESULT_A),
        BgTaskKind::Shell,
    );
    store
        .append_reminder_if_absent(&child_id, id, &reminder)
        .await
        .unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager),
        ..Default::default()
    });
    let resumed =
        SessionFactory::resume_subagent(Some(&parent), resume_config(store, child_id.clone()))
            .await
            .expect("冷恢复必须允许从历史开始新执行");
    assert_eq!(resumed.child_thread_id, child_id);
    assert!(resumed.session.transcript().read().get(id).is_some());
    assert_eq!(resumed.session.async_result_mailbox().pending_count(), 0);
    assert!(resumed
        .session
        .subagent_host()
        .unwrap()
        .task_manager
        .as_ref()
        .unwrap()
        .registry()
        .list_tasks_full()
        .is_empty());
}

async fn confirmed_handoff_terminal_write_failure(background: bool) {
    let store = MockSessionResources::new();
    let child_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = child_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&child_id, "error")
        .await
        .unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let child_manager = Arc::new(TaskManager::new());
    register_shell(&child_manager, "status-shell");
    let owner = ParkedChildOwner::new(
        child_id.clone(),
        "parent-thread".into(),
        child_manager,
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    parent_manager.register_child_owner(owner.clone()).unwrap();
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager.clone()),
        ..Default::default()
    });
    let mut config = resume_config_with(
        store.clone(),
        child_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(FailWithoutResidual),
            "fixture-scripted",
        ),
        if background {
            SubagentRunMode::Background
        } else {
            SubagentRunMode::Sync
        },
        Some(parent_manager),
        None,
    );
    let stopped_store = store.clone();
    config.on_subagent_stop = Some(Arc::new(move |_, _, _, _, _| {
        stopped_store.restrict_to_history_read_only();
    }));
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let result_tx = parking_lot::Mutex::new(Some(result_tx));
    config.on_bg_complete = Some(Arc::new(move |result, _| {
        if let Some(sender) = result_tx.lock().take() {
            let _ = sender.send(result.clone());
        }
        Ok(())
    }));
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let finished_tx = parking_lot::Mutex::new(Some(finished_tx));
    config.deregister_runtime = Some(Arc::new(move |_| {
        if let Some(sender) = finished_tx.lock().take() {
            let _ = sender.send(());
        }
    }));
    let first = SessionFactory::resume_subagent(Some(&parent), config).await;
    if background {
        first.expect("后台执行必须启动");
        let result = tokio::time::timeout(Duration::from_secs(10), result_rx)
            .await
            .expect("终态写失败必须交付")
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("terminal status write failed"));
    } else {
        let error = first.err().expect("终态写失败必须返回错误");
        assert!(error.to_string().contains("terminal_status_persist_failed"));
    }
    tokio::time::timeout(Duration::from_secs(10), finished_rx)
        .await
        .expect("旧执行必须完成")
        .unwrap();
    assert!(owner.receipt().is_some(), "交接必须先成功提交");
    assert!(owner.pending_receipt().is_none());
    assert!(owner
        .failure()
        .unwrap()
        .contains("terminal_status_persist_failed"));
    let error = SessionFactory::resume_subagent(Some(&parent), resume_config(store, child_id))
        .await
        .err()
        .expect("终态写入失败必须拒绝 resume")
        .to_string();
    assert!(error.contains("settlement is unconfirmed"), "{error}");
}

/// [回归测试] 装配后后台注册失败不能把已有 Blocked 责任提前改成 Ready。
#[tokio::test]
async fn test_failed_resume_registration_keeps_mailbox_blocked() {
    use crate::session::MessageSource;

    let store = MockSessionResources::new();
    let child_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = child_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&child_id, "error")
        .await
        .unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let owner = ParkedChildOwner::new(
        child_id.clone(),
        "parent-thread".into(),
        Arc::new(TaskManager::new()),
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    let id = terminal_delivery_id("preflight-task", "terminal");
    let reminder = crate::session::async_router::background_result_reminder(
        &shell_result("preflight-task", RESULT_A),
        BgTaskKind::Shell,
    );
    owner
        .mailbox
        .accept(id, &reminder, MessageSource::ShellComplete)
        .unwrap();
    owner
        .mailbox
        .mark_canonical_ready(id, &reminder, MessageSource::ShellComplete, None)
        .unwrap();
    owner.mailbox.block_uncovered("reason", "模型失败");
    parent_manager.register_child_owner(owner.clone()).unwrap();
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager),
        ..Default::default()
    });
    let config = resume_config_with(
        store.clone(),
        child_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        SubagentRunMode::Background,
        None,
        None,
    );
    let error = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .err()
        .expect("缺少后台任务 manager 必须拒绝注册")
        .to_string();
    assert!(error.contains("no task manager configured"), "{error}");
    assert_eq!(owner.mailbox.state_of(&id), Some(AsyncResultState::Blocked));
    assert_eq!(
        store.load_meta(&child_id).await.unwrap().agent_status,
        AgentStatus::Error
    );
}

/// [回归测试] 已确认交接后，同步终态状态写入失败仍拒绝 resume。
#[tokio::test(start_paused = true)]
async fn test_sync_confirmed_handoff_terminal_write_failure_blocks_resume() {
    confirmed_handoff_terminal_write_failure(false).await;
}

/// [回归测试] 已确认交接后，后台终态状态写入失败仍拒绝 resume。
#[tokio::test(start_paused = true)]
async fn test_background_confirmed_handoff_terminal_write_failure_blocks_resume() {
    confirmed_handoff_terminal_write_failure(true).await;
}

/// [回归测试] 后台 resume 已开始但未结算即被强制取消时，不得保留 Ready 责任。
#[tokio::test(start_paused = true)]
async fn test_background_forced_abort_reblocks_unsettled_mailbox() {
    use crate::session::MessageSource;

    let store = MockSessionResources::new();
    let child_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = child_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&child_id, "error")
        .await
        .unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let owner = ParkedChildOwner::new(
        child_id.clone(),
        "parent-thread".into(),
        Arc::new(TaskManager::new()),
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    let blocked_id = terminal_delivery_id("blocked-before-resume", "terminal");
    let reminder = crate::session::async_router::background_result_reminder(
        &shell_result("blocked-before-resume", RESULT_A),
        BgTaskKind::Shell,
    );
    owner
        .mailbox
        .accept(blocked_id, &reminder, MessageSource::ShellComplete)
        .unwrap();
    owner
        .mailbox
        .mark_canonical_ready(blocked_id, &reminder, MessageSource::ShellComplete, None)
        .unwrap();
    owner.mailbox.block_uncovered("reason", "模型失败");
    let delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::session::bg_complete::queue_terminal_delivery_with_mailbox(
            owner.queue.clone(),
            owner.mailbox.clone(),
        ),
    );
    delivery(&shell_result("receive-gate", RESULT_B), BgTaskKind::Shell).unwrap();
    let queued_id = terminal_delivery_id("receive-gate", "terminal");
    parent_manager.register_child_owner(owner.clone()).unwrap();
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager.clone()),
        ..Default::default()
    });
    let (entered, release) = store.gate_next_reminder_append();
    let config = resume_config_with(
        store,
        child_id,
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(EchoLLM),
            "fixture-scripted",
        ),
        SubagentRunMode::Background,
        Some(parent_manager.clone()),
        None,
    );
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("后台恢复必须启动");
    tokio::time::timeout(Duration::from_secs(10), entered)
        .await
        .expect("Receive 必须卡在规范提交")
        .unwrap();
    assert_eq!(
        owner.mailbox.state_of(&blocked_id),
        Some(AsyncResultState::Ready)
    );
    assert_eq!(
        owner.mailbox.state_of(&queued_id),
        Some(AsyncResultState::Queued)
    );
    parent_manager
        .cancel(spawned.task_id.as_deref().unwrap())
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(4)).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while owner.failure().is_none() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("强制 abort 必须留下未确认结算");
    assert!(release.send(()).is_err(), "持久化 future 必须已被强制中止");
    assert!(owner.check_resume().unwrap_err().contains("unconfirmed"));
    assert_eq!(
        owner.mailbox.state_of(&blocked_id),
        Some(AsyncResultState::Blocked),
        "未结算的强制取消不能留下 Ready 自动激活"
    );
    assert_eq!(
        owner.mailbox.state_of(&queued_id),
        Some(AsyncResultState::Queued),
        "尚未规范提交的责任仍由队列负责"
    );
}
