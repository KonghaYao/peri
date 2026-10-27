use super::*;

/// 旧执行的 Stop 留在 owner 队列：真实 resume 复用队列后必须抑制它，新 run 仍推理。
#[tokio::test]
async fn test_resume_suppresses_stop_intent_from_previous_execution() {
    use crate::agent::async_tasks::TaskManager;
    use crate::messages::BaseMessage;
    use crate::session::queue::{MessageSource, QueuedMessage};
    use crate::session::subagent::{ParkedChildOwner, SubagentHost};
    use crate::session::{FrozenContext, MessageQueue, Session};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct CountingAnswerLlm(Arc<AtomicUsize>);

    impl CountingAnswerLlm {
        async fn respond(
            &self,
            _request: peri_model::ModelRequest,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            crate::session::test_resources::mock::model::text_events("resumed answer")
        }
    }
    crate::fixture_model_impl!(CountingAnswerLlm);

    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = thread_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&thread_id, "error")
        .await
        .unwrap();

    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    let child_manager = Arc::new(TaskManager::new());
    let owner = ParkedChildOwner::new(
        thread_id.clone(),
        "parent-thread".into(),
        child_manager,
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    let previous_execution =
        Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None)
            .start_turn()
            .execution_binding();
    let reminder = crate::session::producer_reminders::trusted_reminder(
        peri_acp_types::system_reminder::ReminderCategory::Lifecycle,
        "hook",
        "post_tool_batch_stop",
        peri_acp_types::system_reminder::ReminderSeverity::Info,
        peri_acp_types::system_reminder::ReminderDelivery::Configurable,
        "old hook stop".into(),
        None,
        serde_json::json!({}),
    );
    owner.queue.push(
        QueuedMessage::system_reminder(
            crate::session::MessageKind::Info,
            MessageSource::HookStopIntent,
            reminder,
        )
        .with_policy(peri_acp_types::session::MessagePolicy::stop_current_run(
            previous_execution,
        )),
    );
    owner.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("resume without old stop"),
    ));
    let parent_manager = Arc::new(TaskManager::new());
    parent_manager.register_child_owner(owner.clone()).unwrap();
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager),
        session_resources: Some(store.clone()),
        ..Default::default()
    });

    let calls = Arc::new(AtomicUsize::new(0));
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(CountingAnswerLlm(calls.clone())),
            "fixture-scripted",
        ),
        SubagentRunMode::Sync,
        None,
        None,
    );
    SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("resume must accept the handed-off owner");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "旧 Stop 不得阻止 resume 新执行推理"
    );
    assert_eq!(owner.queue.len(), 0);
    assert_eq!(
        owner.queue.suppressed_messages().len(),
        1,
        "旧 Stop 必须作为过期控制消息显式记录"
    );
}

/// 真实 resume factory/claim 复用 handoff owner 的原队列与 manager。
#[tokio::test]
async fn test_resume_factory_reuses_handoff_owner_queue_and_manager() {
    use crate::agent::async_tasks::TaskManager;
    use crate::messages::BaseMessage;
    use crate::session::queue::{MessageSource, QueuedMessage};
    use crate::session::subagent::{ParkedChildOwner, SubagentHost};
    use crate::session::{FrozenContext, MessageQueue, Session};

    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = thread_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    store
        .update_thread_status(&thread_id, "error")
        .await
        .unwrap();

    let parent_manager = Arc::new(TaskManager::new());
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    let child_manager = Arc::new(TaskManager::new());
    let owner = ParkedChildOwner::new(
        thread_id.clone(),
        "parent-thread".into(),
        child_manager.clone(),
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    owner.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("late handoff context"),
    ));
    parent_manager.register_child_owner(owner.clone()).unwrap();
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager),
        session_resources: Some(store.clone()),
        ..Default::default()
    });

    let spawned = SessionFactory::resume_subagent(
        Some(&parent),
        resume_config(store.clone(), thread_id.clone()),
    )
    .await
    .expect("factory resume must accept the handed-off owner");
    assert_eq!(spawned.child_thread_id, thread_id);
    assert_eq!(owner.queue.len(), 0, "恢复必须复用并消费原 owner 队列");
    assert!(
        owner.check_resume().is_ok(),
        "恢复完成后 owner 必须释放执行准入"
    );
}

#[derive(Clone)]
struct FailWithQueuedInput {
    queue: crate::session::MessageQueue,
}

impl FailWithQueuedInput {
    async fn respond(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        self.queue.push(crate::session::QueuedMessage::prompt(
            crate::session::MessageSource::UserInput,
            BaseMessage::human("未消费的失败后输入"),
        ));
        vec![Err(peri_model::ModelError::http_status(
            400,
            "fixture",
            None::<&str>,
        ))]
    }
}
crate::fixture_model_impl!(FailWithQueuedInput);

#[derive(Clone)]
struct ObserveResumedInput(Arc<parking_lot::Mutex<Vec<String>>>);

impl ObserveResumedInput {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        _cancellation: CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        self.0.lock().extend(
            crate::session::test_resources::mock::model::base_messages(&request)
                .iter()
                .map(|message| message.content().to_string()),
        );
        crate::session::test_resources::mock::model::text_events("恢复后的回答")
    }
}
crate::fixture_model_impl!(ObserveResumedInput);

async fn test_failed_model_handoff_then_resume(background: bool, fail_drain: bool) {
    use crate::session::subagent::{ChildHandoffReason, ParkedChildOwner, SubagentFailure};
    use crate::session::{FrozenContext, MessageQueue, Session};
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as _};
    use std::time::Duration;

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
    child_manager
        .register(BgTaskRegistration {
            task_id: "retained-shell".into(),
            kind: BgTaskKind::Shell,
            summary: "测试中的后台任务".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
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
        session_resources: Some(store.clone()),
        ..Default::default()
    });
    let mode = if background {
        SubagentRunMode::Background
    } else {
        SubagentRunMode::Sync
    };
    let mut config = resume_config_with(
        store.clone(),
        child_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(FailWithQueuedInput {
                queue: owner.queue.clone(),
            }),
            "fixture-scripted",
        ),
        mode,
        Some(parent_manager.clone()),
        None,
    );
    if fail_drain {
        config.langfuse_bridge = Some(Arc::new(drain_tests::FailOnStopBridge));
        config.parent_agent_id = Some(AgentId::new());
    }
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let finished_tx = parking_lot::Mutex::new(Some(finished_tx));
    config.deregister_runtime = Some(Arc::new(move |_| {
        if let Some(sender) = finished_tx.lock().take() {
            let _ = sender.send(());
        }
    }));
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let result_tx = parking_lot::Mutex::new(Some(result_tx));
    config.on_bg_complete = Some(Arc::new(move |result, _| {
        if let Some(sender) = result_tx.lock().take() {
            let _ = sender.send(result.clone());
        }
        Ok(())
    }));
    let first = SessionFactory::resume_subagent(Some(&parent), config).await;
    if background {
        assert_eq!(first.expect("后台执行须成功启动").child_thread_id, child_id);
        let result = tokio::time::timeout(Duration::from_secs(10), result_rx)
            .await
            .expect("后台失败必须完成")
            .unwrap();
        assert!(!result.success);
        assert!(!result.timed_out);
        assert_eq!(result.child_thread_id.as_deref(), Some(child_id.as_str()));
    } else {
        let error = first.err().expect("模型失败必须返回错误");
        let failure = error
            .downcast_ref::<SubagentFailure>()
            .expect("保留类型化错误");
        if !fail_drain {
            assert!(failure.handoff().is_some(), "失败必须携带已提交交接");
        }
    }
    tokio::time::timeout(Duration::from_secs(10), finished_rx)
        .await
        .expect("旧执行必须完成事件排空和 permit 结算")
        .unwrap();
    let receipt = owner.receipt().expect("必须保留已确认交接");
    assert_eq!(receipt.reason, ChildHandoffReason::ExecutionFailed);
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "retained-shell"));
    assert!(owner.pending_receipt().is_none());
    assert!(owner.queue.has_required(), "失败期间到达的输入必须保留");
    assert_eq!(child_manager.active_count(), 1);
    if fail_drain {
        assert!(owner.failure().unwrap().contains("event drain failed"));
        let error = SessionFactory::resume_subagent(Some(&parent), resume_config(store, child_id))
            .await
            .err()
            .expect("事件排空失败必须阻止 resume")
            .to_string();
        assert!(error.contains("settlement is unconfirmed"), "{error}");
        return;
    }

    let observed = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut resumed_config = resume_config_with(
        store.clone(),
        child_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            Arc::new(ObserveResumedInput(observed.clone())),
            "fixture-scripted",
        ),
        SubagentRunMode::Sync,
        Some(parent_manager.clone()),
        None,
    );
    let (resumed_tx, resumed_rx) = tokio::sync::oneshot::channel();
    let resumed_tx = parking_lot::Mutex::new(Some(resumed_tx));
    resumed_config.deregister_runtime = Some(Arc::new(move |_| {
        if let Some(sender) = resumed_tx.lock().take() {
            let _ = sender.send(());
        }
    }));
    let resumed = tokio::time::timeout(
        Duration::from_secs(130),
        SessionFactory::resume_subagent(Some(&parent), resumed_config),
    )
    .await
    .expect("新执行必须在有界窗口内返回")
    .expect("已可靠交接的失败必须允许显式 resume");
    tokio::time::timeout(Duration::from_secs(10), resumed_rx)
        .await
        .expect("新执行必须完成旧事件排空")
        .unwrap();
    assert_eq!(resumed.child_thread_id, child_id);
    let resumed_host = resumed.session.subagent_host().unwrap();
    assert!(Arc::ptr_eq(
        resumed_host.task_manager.as_ref().unwrap(),
        &child_manager
    ));
    assert!(Arc::ptr_eq(
        resumed_host.child_owner.as_ref().unwrap(),
        &owner
    ));
    assert!(observed
        .lock()
        .iter()
        .any(|text| text.contains("未消费的失败后输入")));
    assert_eq!(child_manager.active_count(), 1, "恢复不得取消原任务");
    assert_eq!(
        child_manager
            .registry()
            .list_tasks_full()
            .iter()
            .filter(|task| task.task_id == "retained-shell")
            .count(),
        1,
        "恢复不得重复注册任务"
    );
}

/// [回归测试] 同步模型失败已完成交接，显式恢复必须保留原资源。
#[tokio::test(start_paused = true)]
async fn test_failed_model_handoff_sync_can_resume_original_owner() {
    test_failed_model_handoff_then_resume(false, false).await;
}

/// [回归测试] 后台模型失败已完成交接，显式恢复必须保留原资源。
#[tokio::test(start_paused = true)]
async fn test_failed_model_handoff_background_can_resume_original_owner() {
    test_failed_model_handoff_then_resume(true, false).await;
}

#[path = "resume_owner_drain_test.rs"]
mod drain_tests;
#[path = "resume_owner_mailbox_test.rs"]
mod mailbox_tests;

/// [回归测试] 真实执行 permit 结算失败时，factory 应给出具体且不含内部错误的拒绝。
#[tokio::test]
async fn test_resume_rejects_unconfirmed_settlement_with_specific_diagnostic() {
    use crate::session::subagent::ParkedChildOwner;
    use crate::session::{FrozenContext, MessageQueue, Session};

    let store = MockSessionResources::new();
    let child_id = uuid::Uuid::now_v7().to_string();
    let mut meta = ThreadMeta::new_at("/tmp", peri_time::now_wall());
    meta.id = child_id.clone();
    meta.parent_thread_id = Some("parent-thread".into());
    store.create_resumable_thread(meta).await.unwrap();
    let parent_manager = Arc::new(TaskManager::new());
    let owner = ParkedChildOwner::new(
        child_id.clone(),
        "parent-thread".into(),
        Arc::new(TaskManager::new()),
        MessageQueue::new(),
        Some(store.clone()),
        None,
    );
    let execution = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None)
        .start_turn()
        .execution_binding();
    owner
        .acquire(execution)
        .unwrap()
        .finish(Err("内部持久化错误内容".into()));
    parent_manager.register_child_owner(owner).unwrap();
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager),
        ..Default::default()
    });
    let error = SessionFactory::resume_subagent(Some(&parent), resume_config(store, child_id))
        .await
        .err()
        .expect("未确认结算必须拒绝恢复")
        .to_string();
    assert!(error.contains("settlement is unconfirmed"), "{error}");
    assert!(!error.contains("内部持久化错误内容"));
}

struct FailingReactLlm;

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for FailingReactLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        Err(anyhow::anyhow!("fixture model failure").into())
    }
}

async fn test_post_handoff_history_flush_failure(background: bool) {
    use crate::agent::events::BackgroundTaskResult;
    use crate::session::subagent::{ParkedChildOwner, SubagentFailure};
    use crate::session::{FrozenContext, MessageTranscript, Session};
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as _};
    use std::time::Duration;

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
    child_manager
        .register(BgTaskRegistration {
            task_id: "flush-shell".into(),
            kind: BgTaskKind::Shell,
            summary: "测试中的后台任务".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    *child.transcript().write() =
        MessageTranscript::new().with_persistence(store.clone(), child_id.clone());
    let owner = ParkedChildOwner::new(
        child_id.clone(),
        "parent-thread".into(),
        child_manager.clone(),
        child.queue().clone(),
        Some(store.clone()),
        None,
    );
    parent_manager.register_child_owner(owner.clone()).unwrap();
    child.set_subagent_host(SubagentHost {
        task_manager: Some(child_manager.clone()),
        child_owner: Some(owner.clone()),
        ..Default::default()
    });
    child.queue().push(crate::session::QueuedMessage::prompt(
        crate::session::MessageSource::UserInput,
        BaseMessage::human("开始执行"),
    ));
    let built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(FailingReactLlm),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        child.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&child_id)),
    );
    let (entered, release) = store.gate_next_reminder_append();
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let finished_tx = parking_lot::Mutex::new(Some(finished_tx));
    let deregister: crate::session::factory::DeregisterRuntimeFn = Arc::new(move |_| {
        if let Some(sender) = finished_tx.lock().take() {
            let _ = sender.send(());
        }
    });
    let sync_run = if background {
        crate::session::subagent::background::spawn_background_subagent(
            "flush-parent-task".into(),
            child_id.clone(),
            "fixture".into(),
            "开始执行".into(),
            "/tmp".into(),
            10,
            None,
            Some(parent_manager.clone()),
            None,
            None,
            Some(store.clone()),
            Some(deregister),
            None,
            None,
            None,
            None,
            None,
            child.config().cancel_token.as_ref().clone(),
            built,
            None,
        )
        .await
        .unwrap();
        None
    } else {
        let run_child = child.clone();
        let run_id = child_id.clone();
        let run_store = store.clone();
        Some(tokio::spawn(async move {
            crate::session::subagent::run_sync::run_sync_subagent(
                &run_id,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                Some(run_store),
                None,
                Some(deregister),
                None,
                None,
                None,
                built,
                run_child,
                None,
                None,
            )
            .await
        }))
    };
    tokio::time::timeout(Duration::from_secs(10), entered)
        .await
        .expect("交接必须到达 canonical 提交")
        .unwrap();
    assert!(child_manager.complete(
        "flush-shell",
        BackgroundTaskResult {
            task_id: "flush-shell".into(),
            agent_name: "shell".into(),
            prompt_summary: "shell".into(),
            success: true,
            output: "done".into(),
            tool_calls_count: 0,
            duration_ms: 0,
            child_thread_id: None,
            timed_out: false,
            subagent_failure: None,
            shell_output: None,
        }
    ));
    child.queue().drain_all();
    let writer = child.transcript().read().persist_tx_handle().unwrap();
    child.transcript().read().shutdown_persistence();
    tokio::time::timeout(Duration::from_secs(10), writer.closed())
        .await
        .expect("测试 writer 必须在提交后续阶段前关闭");
    release.send(()).unwrap();
    if let Some(run) = sync_run {
        let error = run.await.unwrap().err().expect("模型错误必须返回");
        let failure = error.downcast_ref::<SubagentFailure>().unwrap();
        assert!(failure.handoff().is_some(), "canonical 交接已提交");
    }
    tokio::time::timeout(Duration::from_secs(10), finished_rx)
        .await
        .expect("旧执行必须完成")
        .unwrap();
    assert!(owner.receipt().is_some(), "交接必须先于后续 flush 失败提交");
    assert!(owner.pending_receipt().is_none());
    assert_eq!(child_manager.active_count(), 0);
    assert!(!owner.queue.has_required());
    assert_eq!(
        owner.failure().as_deref(),
        Some("child history flush is unconfirmed")
    );
    let parent = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some("parent-thread".into()),
    );
    parent.set_subagent_host(SubagentHost {
        task_manager: Some(parent_manager),
        ..Default::default()
    });
    let error = SessionFactory::resume_subagent(Some(&parent), resume_config(store, child_id))
        .await
        .err()
        .expect("交接后的 flush 失败必须阻止 resume")
        .to_string();
    assert!(error.contains("settlement is unconfirmed"), "{error}");
}

/// [回归测试] 交接确认后 history flush 失败，不能放行同步执行。
#[tokio::test(start_paused = true)]
async fn test_sync_post_handoff_history_flush_failure_blocks_resume() {
    test_post_handoff_history_flush_failure(false).await;
}

/// [回归测试] 交接确认后 history flush 失败，不能放行后台执行。
#[tokio::test(start_paused = true)]
async fn test_background_post_handoff_history_flush_failure_blocks_resume() {
    test_post_handoff_history_flush_failure(true).await;
}

#[derive(Clone)]
struct FailWithoutResidual;

impl FailWithoutResidual {
    async fn respond(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        vec![Err(peri_model::ModelError::http_status(
            400,
            "fixture",
            None::<&str>,
        ))]
    }
}
crate::fixture_model_impl!(FailWithoutResidual);

async fn test_failed_without_receipt_resume(background: bool, late_obligations: bool) {
    use crate::session::subagent::{ParkedChildOwner, SubagentFailure};
    use crate::session::{FrozenContext, MessageQueue, Session};
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as _};
    use std::time::Duration;

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
    if late_obligations {
        let late_manager = child_manager.clone();
        let late_queue = owner.queue.clone();
        config.on_subagent_stop = Some(Arc::new(move |_, _, _, _, _| {
            late_manager
                .register(BgTaskRegistration {
                    task_id: "late-shell".into(),
                    kind: BgTaskKind::Shell,
                    summary: "结算期间新到达的任务".into(),
                    pid: None,
                    kill: Some(Box::new(|| {})),
                })
                .unwrap();
            late_queue.push(crate::session::QueuedMessage::prompt(
                crate::session::MessageSource::UserInput,
                BaseMessage::human("结算期间到达的输入"),
            ));
        }));
    }
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let finished_tx = parking_lot::Mutex::new(Some(finished_tx));
    config.deregister_runtime = Some(Arc::new(move |_| {
        if let Some(sender) = finished_tx.lock().take() {
            let _ = sender.send(());
        }
    }));
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let result_tx = parking_lot::Mutex::new(Some(result_tx));
    config.on_bg_complete = Some(Arc::new(move |result, _| {
        if let Some(sender) = result_tx.lock().take() {
            let _ = sender.send(result.clone());
        }
        Ok(())
    }));
    let first = SessionFactory::resume_subagent(Some(&parent), config).await;
    if background {
        first.expect("后台执行须成功启动");
        let result = tokio::time::timeout(Duration::from_secs(10), result_rx)
            .await
            .expect("后台失败必须完成")
            .unwrap();
        assert!(!result.success);
    } else {
        let error = first.err().expect("模型失败必须返回错误");
        let failure = error.downcast_ref::<SubagentFailure>().unwrap();
        assert!(
            failure.handoff().is_none(),
            "模型失败时尚无义务，不能伪造交接"
        );
    }
    tokio::time::timeout(Duration::from_secs(10), finished_rx)
        .await
        .expect("旧执行必须完成")
        .unwrap();
    assert!(owner.receipt().is_none());
    if !late_obligations {
        assert!(owner.failure().is_none());
        assert_eq!(child_manager.active_count(), 0);
        let resumed =
            SessionFactory::resume_subagent(Some(&parent), resume_config(store, child_id.clone()))
                .await
                .expect("没有残余义务的失败应允许新执行");
        assert_eq!(resumed.child_thread_id, child_id);
        return;
    }
    assert_eq!(child_manager.active_count(), 1);
    assert!(owner.queue.has_required());
    assert_eq!(
        owner.failure().as_deref(),
        Some("child settlement is unconfirmed after execution failure")
    );
    let error = SessionFactory::resume_subagent(Some(&parent), resume_config(store, child_id))
        .await
        .err()
        .expect("无交接回执的未确认义务必须阻止 resume")
        .to_string();
    assert!(error.contains("settlement is unconfirmed"), "{error}");
}

/// [回归测试] 无 receipt 的同步失败在结算期间出现新义务时必须阻断。
#[tokio::test(start_paused = true)]
async fn test_sync_failed_without_receipt_late_obligations_block_resume() {
    test_failed_without_receipt_resume(false, true).await;
}

/// [回归测试] 无 receipt 的后台失败在结算期间出现新义务时必须阻断。
#[tokio::test(start_paused = true)]
async fn test_background_failed_without_receipt_late_obligations_block_resume() {
    test_failed_without_receipt_resume(true, true).await;
}

/// [回归测试] 无 receipt 且无遗留义务的同步失败可以恢复。
#[tokio::test(start_paused = true)]
async fn test_sync_failed_without_receipt_or_obligations_can_resume() {
    test_failed_without_receipt_resume(false, false).await;
}

/// [回归测试] 无 receipt 且无遗留义务的后台失败可以恢复。
#[tokio::test(start_paused = true)]
async fn test_background_failed_without_receipt_or_obligations_can_resume() {
    test_failed_without_receipt_resume(true, false).await;
}

#[derive(Clone)]
struct FailWithUnwritableStore(Arc<MockSessionResources>);

impl FailWithUnwritableStore {
    async fn respond(
        &self,
        _request: peri_model::ModelRequest,
        _cancellation: CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        self.0.restrict_to_history_read_only();
        vec![Err(peri_model::ModelError::http_status(
            400,
            "fixture",
            None::<&str>,
        ))]
    }
}
crate::fixture_model_impl!(FailWithUnwritableStore);

async fn test_unconfirmed_handoff_blocks_factory_resume(background: bool) {
    use crate::session::subagent::ParkedChildOwner;
    use crate::session::{FrozenContext, MessageQueue, Session};
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as _};
    use std::time::Duration;

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
    child_manager
        .register(BgTaskRegistration {
            task_id: "unconfirmed-shell".into(),
            kind: BgTaskKind::Shell,
            summary: "测试中的后台任务".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
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
            Arc::new(FailWithUnwritableStore(store.clone())),
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
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let finished_tx = parking_lot::Mutex::new(Some(finished_tx));
    config.deregister_runtime = Some(Arc::new(move |_| {
        if let Some(sender) = finished_tx.lock().take() {
            let _ = sender.send(());
        }
    }));
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let result_tx = parking_lot::Mutex::new(Some(result_tx));
    config.on_bg_complete = Some(Arc::new(move |result, _| {
        if let Some(sender) = result_tx.lock().take() {
            let _ = sender.send(result.clone());
        }
        Ok(())
    }));
    let first = SessionFactory::resume_subagent(Some(&parent), config).await;
    if background {
        first.expect("后台执行须成功启动");
        let result = tokio::time::timeout(Duration::from_secs(10), result_rx)
            .await
            .expect("后台失败必须完成")
            .unwrap();
        assert!(!result.success);
    } else {
        assert!(first.is_err(), "持久化失败必须返回错误");
    }
    tokio::time::timeout(Duration::from_secs(10), finished_rx)
        .await
        .expect("旧执行必须完成")
        .unwrap();
    assert!(owner.pending_receipt().is_some(), "未确认交接身份必须保留");
    assert!(owner.receipt().is_none());
    let error = SessionFactory::resume_subagent(Some(&parent), resume_config(store, child_id))
        .await
        .err()
        .expect("未确认交接必须阻止 resume")
        .to_string();
    assert!(error.contains("handoff is unconfirmed"), "{error}");
}

/// [回归测试] 同步交接真实写入失败时，factory 明确拒绝恢复。
#[tokio::test(start_paused = true)]
async fn test_sync_unconfirmed_handoff_blocks_factory_resume() {
    test_unconfirmed_handoff_blocks_factory_resume(false).await;
}

/// [回归测试] 后台交接真实写入失败时，factory 明确拒绝恢复。
#[tokio::test(start_paused = true)]
async fn test_background_unconfirmed_handoff_blocks_factory_resume() {
    test_unconfirmed_handoff_blocks_factory_resume(true).await;
}
