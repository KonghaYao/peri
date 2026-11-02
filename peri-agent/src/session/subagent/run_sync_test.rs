use super::*;
use crate::agent::react::{ReactLLM, Reasoning, StreamingContext};
use crate::middleware::chain::MiddlewareChain;
use crate::session::queue::{MessageSource, QueuedMessage};
use crate::session::subagent::{
    agent_id_from_child_thread, build_v2_subagent_context, ChildRunOutcome, ParkedChildOwner,
    SubagentHost, V2SubagentContext,
};
use crate::session::{FrozenContext, MessageTranscript};
use crate::tools::BaseTool;
use peri_acp_types::tasks::{BgTaskRegistration, OnBgCompleteFn, TaskManager as TaskManagerPort};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn child(manager: Arc<TaskManager>) -> Arc<Session> {
    let session = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(uuid::Uuid::now_v7().to_string()),
    );
    session.set_subagent_host(SubagentHost {
        task_manager: Some(manager),
        ..Default::default()
    });
    session
}

fn bound_child(manager: Arc<TaskManager>) -> (Arc<Session>, Arc<ParkedChildOwner>) {
    let (session, owner, _) = bound_child_with_store(manager);
    (session, owner)
}

fn bound_child_with_store(
    manager: Arc<TaskManager>,
) -> (
    Arc<Session>,
    Arc<ParkedChildOwner>,
    Arc<crate::session::test_resources::mock::MockSessionResources>,
) {
    let child_id = uuid::Uuid::now_v7().to_string();
    let session = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    let store = crate::session::test_resources::mock::MockSessionResources::new();
    *session.transcript().write() =
        MessageTranscript::new().with_persistence(store.clone(), child_id.clone());
    let owner = ParkedChildOwner::new(
        child_id.clone(),
        child_id.clone(),
        manager.clone(),
        session.queue().clone(),
        Some(store.clone()),
        None,
    );
    owner.set_writer(session.transcript().read().persist_tx_handle());
    manager.register_child_owner(owner.clone()).unwrap();
    session.set_subagent_host(SubagentHost {
        task_manager: Some(manager),
        child_owner: Some(owner.clone()),
        ..Default::default()
    });
    (session, owner, store)
}

struct AnswerLlm(&'static str);

struct FailingLlm;

#[async_trait::async_trait]
impl ReactLLM for FailingLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        Err(anyhow::anyhow!("model failed").into())
    }
}

struct ProbeLlm(Arc<()>);

#[async_trait::async_trait]
impl ReactLLM for ProbeLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        let _keep_alive = &self.0;
        Ok(Reasoning::with_answer("", "probe answer"))
    }
}

fn probe_context(session: &Arc<Session>, marker: Arc<()>) -> V2SubagentContext {
    let id = session.store().thread_id.clone().unwrap();
    build_v2_subagent_context(
        Some(session.clone()),
        Box::new(ProbeLlm(marker)),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        session.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&id)),
    )
}

#[async_trait::async_trait]
impl ReactLLM for AnswerLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        Ok(Reasoning::with_answer("", self.0))
    }
}

fn context(session: &Arc<Session>, answer: &'static str) -> V2SubagentContext {
    let id = session.store().thread_id.clone().unwrap();
    build_v2_subagent_context(
        Some(session.clone()),
        Box::new(AnswerLlm(answer)),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        session.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&id)),
    )
}

/// 阻塞在模型调用中、直到测试放行（或任务被丢弃）的假模型。
struct BlockingLlm {
    entered: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl ReactLLM for BlockingLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        self.entered.notify_one();
        self.gate.notified().await;
        Ok(Reasoning::with_answer("", "released"))
    }
}

fn blocking_context(
    session: &Arc<Session>,
    entered: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::Notify>,
) -> V2SubagentContext {
    let id = session.store().thread_id.clone().unwrap();
    build_v2_subagent_context(
        Some(session.clone()),
        Box::new(BlockingLlm { entered, gate }),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        session.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&id)),
    )
}

fn shell_task(manager: &TaskManager, id: &str) -> peri_acp_types::tasks::TaskRegistrationTicket {
    TaskManagerPort::register(
        manager,
        BgTaskRegistration {
            task_id: id.into(),
            kind: peri_acp_types::tasks::BgTaskKind::Shell,
            summary: "sleep 300".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap()
}

fn shell_result(id: &str) -> crate::agent::events::BackgroundTaskResult {
    crate::agent::events::BackgroundTaskResult {
        task_id: id.into(),
        agent_name: "shell".into(),
        prompt_summary: "shell".into(),
        success: true,
        output: "done".into(),
        tool_calls_count: 0,
        duration_ms: 0,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    }
}

/// 截止前完成：同步调用直接返回 Settled，不登记父 sync 任务。
#[tokio::test(start_paused = true)]
async fn settled_sync_child_returns_finished_without_parent_task() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let (session, _owner) = bound_child(manager.clone());
    let ticket = shell_task(&manager, "shell-task");
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session.store().thread_id.clone().unwrap();
    let task_id = new_sync_task_id();
    let run_id = child_id.clone();
    let call = tokio::spawn(run_sync_with_registered_handoff(
        session.clone(),
        child_id,
        "fixture".into(),
        Some(parent_manager.clone()),
        Some(task_id),
        Some(Arc::new(|_, _| Ok(()))),
        move |handoff, _| async move {
            run_sync_subagent(
                &run_id,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                built,
                session,
                None,
                Some(handoff),
            )
            .await
        },
    ));
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    manager.confirm_external_execution_stopped(&ticket);
    assert!(manager.complete(&ticket, shell_result("shell-task")));
    let completion = call.await.unwrap().unwrap();
    let SyncCompletion::Finished(outcome) = completion else {
        panic!("settled run must not detach");
    };
    assert!(matches!(outcome, ChildRunOutcome::Settled), "{outcome:?}");
    assert_eq!(manager.active_count(), 0);
    assert!(manager
        .snapshot()
        .tasks
        .iter()
        .all(|task| matches!(task.status.as_str(), "completed" | "failed")));
}

#[tokio::test]
async fn failed_receipt_before_cleanup_preserves_typed_failure_without_detaching() {
    let session = child(Arc::new(TaskManager::new()));
    let parent_manager = Arc::new(TaskManager::new());
    let child_id = session.store().thread_id.clone().unwrap();
    let receipt = ChildHandoffReceipt {
        child_thread_id: child_id.clone(),
        execution: session.start_turn().execution_binding(),
        owner_session_id: child_id.clone(),
        reason: ChildHandoffReason::ExecutionFailed,
        tasks: Vec::new(),
        message_ids: Vec::new(),
        message_sequences: Vec::new(),
        unprocessed_tool_calls: vec!["last-act-call".into()],
        canonical_message_id: peri_acp_types::messages::MessageId::new(),
    };
    let expected_receipt_id = receipt.canonical_message_id;
    let deliveries = Arc::new(AtomicUsize::new(0));
    let observed_deliveries = deliveries.clone();
    let (cleanup_done, cleanup_ready) = oneshot::channel();
    let run_child_id = child_id.clone();
    let mut completion = Box::pin(run_sync_with_registered_handoff(
        session,
        child_id.clone(),
        "fixture".into(),
        Some(parent_manager.clone()),
        Some(new_sync_task_id()),
        Some(Arc::new(move |_, _| {
            observed_deliveries.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })),
        move |handoff, _| async move {
            handoff.send(receipt.clone()).unwrap();
            cleanup_ready.await.unwrap();
            Err(Box::new(SubagentFailure::failed_with_handoff(
                run_child_id,
                "fixture",
                crate::error::AgentError::MaxIterationsExceeded(400),
                receipt,
            )) as Box<dyn std::error::Error + Send + Sync>)
        },
    ));

    // Poll through receipt delivery while lifecycle cleanup is deliberately
    // blocked. The transport must not turn an already-known failure into Ok.
    assert!(futures::poll!(completion.as_mut()).is_pending());
    assert!(parent_manager.snapshot().tasks.is_empty());
    cleanup_done.send(()).unwrap();
    let error = match completion.await {
        Err(error) => error,
        Ok(_) => panic!("failed execution must retain its original error"),
    };
    let failure = error.downcast_ref::<SubagentFailure>().unwrap();
    assert_eq!(failure.child_thread_id(), child_id);
    assert!(matches!(
        failure.error(),
        crate::error::AgentError::MaxIterationsExceeded(400)
    ));
    let residual = failure.handoff().unwrap();
    assert_eq!(residual.canonical_message_id, expected_receipt_id);
    assert_eq!(residual.unprocessed_tool_calls, ["last-act-call"]);
    assert!(parent_manager.snapshot().tasks.is_empty());
    assert_eq!(deliveries.load(Ordering::SeqCst), 0);
}

/// 截止后交接：父结果 failed、保留 child/task 引用，registry 接管受阻交付。
#[tokio::test(start_paused = true)]
async fn handoff_sync_child_returns_pending_and_registry_retries_delivery() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let (session, _owner) = bound_child(manager.clone());
    let ticket = shell_task(&manager, "shell-task");
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session.store().thread_id.clone().unwrap();
    let sync_state = session.subagent_host().unwrap().close_state.clone();
    let close_session = session.clone();
    let task_id = new_sync_task_id();
    let attempts = Arc::new(AtomicUsize::new(0));
    let callback_attempts = attempts.clone();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = parking_lot::Mutex::new(Some(delivered_tx));
    let delivery: OnBgCompleteFn = Arc::new(move |result, _| {
        if callback_attempts.fetch_add(1, Ordering::SeqCst) < 2 {
            return Err("transient parent rejection".into());
        }
        if let Some(sender) = delivered_tx.lock().take() {
            let _ = sender.send(result.clone());
        }
        Ok(())
    });
    let run_id = child_id.clone();
    let call = tokio::spawn(run_sync_with_registered_handoff(
        session.clone(),
        child_id.clone(),
        "fixture".into(),
        Some(parent_manager.clone()),
        Some(task_id.clone()),
        Some(delivery),
        move |handoff, _| async move {
            run_sync_subagent(
                &run_id,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                built,
                session,
                None,
                Some(handoff),
            )
            .await
        },
    ));
    let completion = call.await.unwrap().unwrap();
    let SyncCompletion::Pending(pending) = completion else {
        panic!("permanent shell must be handed off");
    };
    assert_eq!(pending.receipt.child_thread_id, child_id);
    assert!(pending
        .pending_tasks
        .iter()
        .any(|task| task.contains("shell-task")));
    assert_eq!(manager.active_count(), 1);
    let delivered = tokio::time::timeout(Duration::from_secs(8), delivered_rx)
        .await
        .expect("registry retry owner must deliver")
        .unwrap();
    assert!(!delivered.success);
    assert!(!delivered.timed_out);
    assert_eq!(delivered.task_id, task_id);
    assert_eq!(
        delivered.child_thread_id.as_deref(),
        Some(child_id.as_str())
    );
    assert!(delivered.output.contains("status: incomplete"));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !sync_state.sync_workers.is_empty() {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wrapper must release its session worker after registry takeover");
    assert!(
        sync_state.sync_delivery_errors().is_empty(),
        "成功重试必须清除此前 delivery 失败记录"
    );
    manager.confirm_external_execution_stopped(&ticket);
    assert!(manager.complete(&ticket, shell_result("shell-task")));
    crate::session::subagent::close_subagent_session_scope(close_session)
        .await
        .expect("成功回执后显式 close 必须成功");
}

/// 没有父投递宿主：不制造幽灵 parent task，等待 typed outcome 后同步返回未完成。
#[tokio::test(start_paused = true)]
async fn handoff_without_parent_owner_finishes_incomplete_without_task() {
    let manager = Arc::new(TaskManager::new());
    let (session, _owner) = bound_child(manager.clone());
    shell_task(&manager, "shell-task");
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session.store().thread_id.clone().unwrap();
    let run_id = child_id.clone();
    let completion = run_sync_with_registered_handoff(
        session.clone(),
        child_id.clone(),
        "fixture".into(),
        None,
        Some(new_sync_task_id()),
        Some(Arc::new(|_, _| Ok(()))),
        move |handoff, _| async move {
            run_sync_subagent(
                &run_id,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                built,
                session,
                None,
                Some(handoff),
            )
            .await
        },
    )
    .await
    .unwrap();
    let SyncCompletion::Finished(ChildRunOutcome::HandedOff { receipt }) = completion else {
        panic!("handoff must return typed incomplete outcome");
    };
    assert_eq!(receipt.child_thread_id, child_id);
    assert_eq!(manager.active_count(), 1);
}

/// 父 scope 关闭时 registry 保留待投递结果；不能报告 Close 成功。
#[tokio::test(start_paused = true)]
async fn parent_close_retains_unconfirmed_delivery_and_reports_incomplete() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let (session, _owner) = bound_child(manager.clone());
    shell_task(&manager, "shell-task");
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session.store().thread_id.clone().unwrap();
    let sync_state = session.subagent_host().unwrap().close_state.clone();
    let task_id = new_sync_task_id();
    let run_id = child_id.clone();
    let call = tokio::spawn(run_sync_with_registered_handoff(
        session.clone(),
        child_id,
        "fixture".into(),
        Some(parent_manager.clone()),
        Some(task_id.clone()),
        Some(Arc::new(|_, _| Err("parent delivery unavailable".into()))),
        move |handoff, _| async move {
            run_sync_subagent(
                &run_id,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                built,
                session,
                None,
                Some(handoff),
            )
            .await
        },
    ));
    assert!(matches!(
        call.await.unwrap().unwrap(),
        SyncCompletion::Pending(_)
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while !parent_manager.has_pending_delivery(&task_id) {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("registry must retain the rejected result");
    tokio::time::timeout(Duration::from_secs(2), async {
        while sync_state.sync_delivery_errors().is_empty() {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("持续失败必须保留 delivery 记录");
    parent_manager.begin_session_close();
    assert!(!parent_manager.wait_session_close().await);
    assert!(parent_manager.has_pending_delivery(&task_id));
}

/// 父注册失败/已 closing：不制造幽灵 sync task，返回 typed 未完成。
#[tokio::test(start_paused = true)]
async fn handoff_with_closing_parent_finishes_incomplete_without_registration() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    parent_manager.begin_session_close();
    let (session, _owner) = bound_child(manager.clone());
    shell_task(&manager, "shell-task");
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session.store().thread_id.clone().unwrap();
    let run_id = child_id.clone();
    let completion = run_sync_with_registered_handoff(
        session.clone(),
        child_id.clone(),
        "fixture".into(),
        Some(parent_manager.clone()),
        Some(new_sync_task_id()),
        Some(Arc::new(|_, _| Ok(()))),
        move |handoff, _| async move {
            run_sync_subagent(
                &run_id,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                built,
                session,
                None,
                Some(handoff),
            )
            .await
        },
    )
    .await
    .unwrap();
    let SyncCompletion::Finished(ChildRunOutcome::HandedOff { receipt }) = completion else {
        panic!("closing parent must return typed incomplete outcome");
    };
    assert_eq!(receipt.child_thread_id, child_id);
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "shell-task"));
    assert_eq!(parent_manager.active_count(), 0, "不得留下幽灵 sync task");
}

/// pending 任务列表为空但仍有未消费消息：wrapper 仍以 receipt 交接，不因空列表 abort。
#[tokio::test(start_paused = true)]
async fn handoff_with_empty_task_list_keeps_retained_messages() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let (session, _owner) = bound_child(manager.clone());
    let ticket = shell_task(&manager, "shell-task");
    session.queue().push(QueuedMessage::info(
        MessageSource::HookStopIntent,
        crate::messages::BaseMessage::human("stop"),
    ));
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session.store().thread_id.clone().unwrap();
    let task_id = new_sync_task_id();
    let run_id = child_id.clone();
    let run_session = session.clone();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = parking_lot::Mutex::new(Some(delivered_tx));
    let call = tokio::spawn(run_sync_with_registered_handoff(
        session.clone(),
        child_id.clone(),
        "fixture".into(),
        Some(parent_manager.clone()),
        Some(task_id.clone()),
        Some(Arc::new(
            move |result: &peri_acp_types::event::BackgroundTaskResult, _| {
                if let Some(sender) = delivered_tx.lock().take() {
                    let _ = sender.send(result.clone());
                }
                Ok(())
            },
        )),
        move |handoff, _| async move {
            run_sync_subagent(
                &run_id,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                built,
                run_session,
                None,
                Some(handoff),
            )
            .await
        },
    ));
    tokio::time::advance(Duration::from_secs(1)).await;
    let delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            session.queue().clone(),
        ),
    );
    let mut result = shell_result("shell-task");
    result.agent_name = "shell".into();
    manager.confirm_external_execution_stopped(&ticket);
    assert!(manager.settle_completed(&ticket, result, delivery).unwrap());
    let completion = call.await.unwrap().unwrap();
    let SyncCompletion::Pending(pending) = completion else {
        panic!("retained message must hand off even with empty task list");
    };
    assert!(pending.receipt.tasks.is_empty());
    assert!(!pending.receipt.message_ids.is_empty());
    let delivered = tokio::time::timeout(Duration::from_secs(5), delivered_rx)
        .await
        .expect("parent result must be delivered")
        .unwrap();
    assert!(!delivered.success);
    assert!(!delivered.timed_out);
    assert_eq!(delivered.task_id, task_id);
}

/// worker 必须释放完整计算对象，而不只是 TaskTracker 计数归零。
#[tokio::test(start_paused = true)]
async fn sync_worker_releases_session_llm_and_context_after_takeover() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let (session, _owner) = bound_child(manager.clone());
    shell_task(&manager, "shell-task");
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let marker = Arc::new(());
    let weak = Arc::downgrade(&marker);
    let built = probe_context(&session, marker);
    let child_id = session.store().thread_id.clone().unwrap();
    let sync_state = session.subagent_host().unwrap().close_state.clone();
    let task_id = new_sync_task_id();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = parking_lot::Mutex::new(Some(delivered_tx));
    let run_id = child_id.clone();
    let call = tokio::spawn(run_sync_with_registered_handoff(
        session.clone(),
        child_id.clone(),
        "fixture".into(),
        Some(parent_manager),
        Some(task_id.clone()),
        Some(Arc::new(
            move |result: &peri_acp_types::event::BackgroundTaskResult, _| {
                if let Some(sender) = delivered_tx.lock().take() {
                    let _ = sender.send(result.clone());
                }
                Ok(())
            },
        )),
        move |handoff, _| async move {
            run_sync_subagent(
                &run_id,
                "fixture",
                "/tmp",
                10,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                built,
                session,
                None,
                Some(handoff),
            )
            .await
        },
    ));
    assert!(matches!(
        call.await.unwrap().unwrap(),
        SyncCompletion::Pending(_)
    ));
    let delivered = tokio::time::timeout(Duration::from_secs(5), delivered_rx)
        .await
        .expect("parent result must be delivered")
        .unwrap();
    assert!(!delivered.success);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !sync_state.sync_workers.is_empty() {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker must end");
    tokio::time::timeout(Duration::from_secs(2), async {
        while weak.upgrade().is_some() {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("LLM/StageContext 必须随 worker 释放");
}

/// 真实 run_sync_subagent：执行错误 + handoff flush 失败时父级保留完整证据。
#[tokio::test(start_paused = true)]
async fn run_sync_handoff_failure_preserves_error_and_receipt() {
    use crate::session::subagent::SubagentFailure;

    let manager = Arc::new(TaskManager::new());
    let (session, owner, store) = bound_child_with_store(manager.clone());
    shell_task(&manager, "shell-task");
    session.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let child_id = session.store().thread_id.clone().unwrap();
    let built = build_v2_subagent_context(
        Some(session.clone()),
        Box::new(FailingLlm),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        session.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&child_id)),
    );
    store.restrict_to_history_read_only();
    let error = run_sync_subagent(
        &child_id, "fixture", "/tmp", 10, None, None, None, None, None, None, None, None, None,
        built, session, None, None,
    )
    .await
    .expect_err("model + handoff write failure must return a typed error");
    let failure = error
        .downcast_ref::<SubagentFailure>()
        .expect("typed SubagentFailure");
    assert!(
        failure
            .error()
            .user_facing_message()
            .contains("model failed"),
        "原执行错误必须保留: {}",
        failure.error().user_facing_message()
    );
    let handoff_failure = failure.handoff_failure().expect("结构化交接失败");
    assert!(
        handoff_failure.contains("handoff_persistence_failed"),
        "{handoff_failure}"
    );
    let flush_failure = failure
        .handoff_flush_failure()
        .expect("flush 失败必须附加到结构化失败");
    assert!(
        flush_failure.contains("append_history") || flush_failure.contains("persistence"),
        "{flush_failure}"
    );
    assert!(failure
        .public_message()
        .contains("handoff history flush failed"));
    let receipt = failure.handoff().expect("冻结 receipt");
    assert_eq!(receipt.child_thread_id, child_id);
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "shell-task"));
    assert!(receipt.unprocessed_tool_calls.is_empty());
    assert!(owner.pending_receipt().is_some());
    assert!(owner.check_resume().is_err(), "未确认交接必须拒绝 resume");
}

#[path = "run_sync_cancellation_test.rs"]
mod cancellation_tests;
