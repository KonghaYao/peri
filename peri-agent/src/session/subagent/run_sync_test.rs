use crate::agent::async_tasks::TaskManager;
use crate::agent::react::{ReactLLM, Reasoning, StreamingContext};
use crate::middleware::chain::MiddlewareChain;
use crate::session::queue::{MessageSource, QueuedMessage};

use crate::session::subagent::run_sync::SyncCompletion;
use crate::session::subagent::terminal_settlement::TerminalMqRoute;
use crate::session::subagent::types::ChildHandoffReason;
use crate::session::subagent::types::ChildHandoffReceipt;
use crate::session::subagent::types::SubagentFailure;
use crate::session::subagent::ChildRunOutcome;
use crate::session::subagent::ParkedChildOwner;
use crate::session::subagent::SubagentHost;
use crate::session::subagent::V2SubagentContext;
use crate::session::Session;
use crate::session::{FrozenContext, MessageTranscript};
use crate::tools::BaseTool;
use peri_acp_types::session_resources::SessionResources;

use peri_acp_types::tasks::{BgTaskRegistration, OnBgCompleteFn, TaskManager as TaskManagerPort};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

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

pub(super) fn bound_child_with_store(
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
    manager
        .register_child_owner(owner.clone())
        .expect("the register child owner fixture result is required for this assertion");
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
    let id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    crate::session::subagent::build_v2_subagent_context(
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
        Some(crate::session::subagent::agent_id_from_child_thread(&id)),
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

pub(super) fn context(session: &Arc<Session>, answer: &'static str) -> V2SubagentContext {
    let id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    crate::session::subagent::build_v2_subagent_context(
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
        Some(crate::session::subagent::agent_id_from_child_thread(&id)),
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
    let id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    crate::session::subagent::build_v2_subagent_context(
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
        Some(crate::session::subagent::agent_id_from_child_thread(&id)),
    )
}

pub(super) fn shell_task(
    manager: &TaskManager,
    id: &str,
) -> peri_acp_types::tasks::TaskRegistrationTicket {
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
    .expect("the register fixture result is required for this assertion")
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
    let (session, _owner, store) = bound_child_with_store(manager.clone());
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    store.register_bound_session(&child_id, "/tmp");
    let ticket = shell_task(&manager, "shell-task");
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let task_id = crate::session::subagent::run_sync::new_sync_task_id();
    let run_id = child_id.clone();
    let run_store = Arc::clone(&store) as Arc<dyn SessionResources>;
    let call = tokio::spawn(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            session.clone(),
            child_id.clone(),
            "fixture".into(),
            Some(parent_manager.clone()),
            Some(task_id),
            Some(Arc::new(|_, _| Ok(()))),
            None,
            move |handoff, _| async move {
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
        ),
    );
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    manager.confirm_external_execution_stopped(&ticket);
    assert!(manager.complete(&ticket, shell_result("shell-task")));
    let completion = call.await.expect("the call fixture result is required for this assertion").expect("settled sync child returns finished without parent task: the completed fixture operation must provide its result payload");
    let SyncCompletion::Finished(outcome) = completion else {
        panic!("settled run must not detach");
    };
    assert!(matches!(outcome, ChildRunOutcome::Settled), "{outcome:?}");
    let meta = store
        .load_session_meta(&child_id)
        .await
        .expect("the load session meta fixture result is required for this assertion");
    assert_eq!(meta.agent_status, peri_acp_types::thread::AgentStatus::Done);
    assert_eq!(meta.work_state, peri_acp_types::thread::WorkState::Idle);
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
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
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
    let mut completion = Box::pin(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            session,
            child_id.clone(),
            "fixture".into(),
            Some(parent_manager.clone()),
            Some(crate::session::subagent::run_sync::new_sync_task_id()),
            Some(Arc::new(move |_, _| {
                observed_deliveries.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
            None,
            move |handoff, _| async move {
                handoff
                    .sender
                    .send(receipt.clone())
                    .expect("handoff sender test receiver must remain available for this delivery");
                cleanup_ready
                    .await
                    .expect("the cleanup ready fixture result is required for this assertion");
                Err(Box::new(SubagentFailure::failed_with_handoff(
                    run_child_id,
                    "fixture",
                    crate::error::AgentError::MaxIterationsExceeded(400),
                    receipt,
                ))
                    as Box<dyn std::error::Error + Send + Sync>)
            },
        ),
    );

    // Poll through receipt delivery while lifecycle cleanup is deliberately
    // blocked. The transport must not turn an already-known failure into Ok.
    assert!(futures::poll!(completion.as_mut()).is_pending());
    assert!(parent_manager.snapshot().tasks.is_empty());
    cleanup_done
        .send(())
        .expect("cleanup done send test receiver must remain available for this delivery");
    let error = match completion.await {
        Err(error) => error,
        Ok(_) => panic!("failed execution must retain its original error"),
    };
    let failure = error
        .downcast_ref::<SubagentFailure>()
        .expect("the <SubagentFailure> fixture result is required for this assertion");
    assert_eq!(failure.child_thread_id(), child_id);
    assert!(matches!(
        failure.error(),
        crate::error::AgentError::MaxIterationsExceeded(400)
    ));
    let residual = failure
        .handoff()
        .expect("the handoff fixture result is required for this assertion");
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
    let (session, _owner, store) = bound_child_with_store(manager.clone());
    let ticket = shell_task(&manager, "shell-task");
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    store.register_bound_session(&child_id, "/tmp");
    let terminal_route = TerminalMqRoute {
        store: Arc::clone(&store) as Arc<dyn SessionResources>,
        target: child_id.clone(),
        mailbox: None,
    };
    let sync_state = session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .clone();
    let close_session = session.clone();
    let task_id = crate::session::subagent::run_sync::new_sync_task_id();
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
    let call = tokio::spawn(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            session.clone(),
            child_id.clone(),
            "fixture".into(),
            Some(parent_manager.clone()),
            Some(task_id.clone()),
            Some(delivery),
            Some(terminal_route),
            move |handoff, _| async move {
                crate::session::subagent::run_sync::run_sync_subagent(
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
        ),
    );
    let completion = call.await.expect("the call fixture result is required for this assertion").expect("handoff sync child returns pending and registry retries delivery: the completed fixture operation must provide its result payload");
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
        .expect("the expect fixture result is required for this assertion");
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
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    let run_id = child_id.clone();
    let completion = crate::session::subagent::run_sync::run_sync_with_registered_handoff(
        session.clone(),
        child_id.clone(),
        "fixture".into(),
        None,
        Some(crate::session::subagent::run_sync::new_sync_task_id()),
        Some(Arc::new(|_, _| Ok(()))),
        None,
        move |handoff, _| async move {
            crate::session::subagent::run_sync::run_sync_subagent(
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
    .expect("the run sync with registered handoff fixture result is required for this assertion");
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
    let (session, _owner, store) = bound_child_with_store(manager.clone());
    shell_task(&manager, "shell-task");
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    store.register_bound_session(&child_id, "/tmp");
    let terminal_route = TerminalMqRoute {
        store: Arc::clone(&store) as Arc<dyn SessionResources>,
        target: child_id.clone(),
        mailbox: None,
    };
    let sync_state = session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .clone();
    let task_id = crate::session::subagent::run_sync::new_sync_task_id();
    let run_id = child_id.clone();
    let call = tokio::spawn(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            session.clone(),
            child_id,
            "fixture".into(),
            Some(parent_manager.clone()),
            Some(task_id.clone()),
            Some(Arc::new(|_, _| Err("parent delivery unavailable".into()))),
            Some(terminal_route),
            move |handoff, _| async move {
                crate::session::subagent::run_sync::run_sync_subagent(
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
        ),
    );
    assert!(matches!(
        call.await.expect("the call fixture result is required for this assertion").expect("parent close retains unconfirmed delivery and reports incomplete: the completed fixture operation must provide its result payload"),
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
    let (session, _owner, store) = bound_child_with_store(manager.clone());
    shell_task(&manager, "shell-task");
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = context(&session, "child answer");
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    store.register_bound_session(&child_id, "/tmp");
    let terminal_route = TerminalMqRoute {
        store: Arc::clone(&store) as Arc<dyn SessionResources>,
        target: child_id.clone(),
        mailbox: None,
    };
    let run_id = child_id.clone();
    let completion = crate::session::subagent::run_sync::run_sync_with_registered_handoff(
        session.clone(),
        child_id.clone(),
        "fixture".into(),
        Some(parent_manager.clone()),
        Some(crate::session::subagent::run_sync::new_sync_task_id()),
        Some(Arc::new(|_, _| Ok(()))),
        Some(terminal_route),
        move |handoff, _| async move {
            crate::session::subagent::run_sync::run_sync_subagent(
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
    .expect("the run sync with registered handoff fixture result is required for this assertion");
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

/// [#425] 同步路径把完整最终文本写入 `SubagentStop.result`，超过 500 字符时
/// 末尾唯一标记必须保留。
#[tokio::test]
async fn settled_sync_subagent_stop_result_keeps_full_text_beyond_500_chars() {
    use peri_acp_types::identity::AgentId;

    const MARKER: &str = "SYNC_SUBAGENT_STOP_TAIL_MARKER";
    let manager = Arc::new(TaskManager::new());
    let (session, _owner, store) = bound_child_with_store(manager);
    let child_id = session
        .store()
        .thread_id
        .clone()
        .expect("the clone fixture result is required for this assertion");
    store.register_bound_session(&child_id, "/tmp");
    session.queue().push_sync(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run sync child"),
    ));
    let long_answer: &'static str =
        Box::leak(format!("{}{MARKER}", "sync-result-".repeat(120)).into_boxed_str());
    let built = context(&session, long_answer);
    let mut observe = built.event_handles.subscribe_observe();
    let run_store: Arc<dyn SessionResources> = store;

    let outcome = crate::session::subagent::run_sync::run_sync_subagent(
        &child_id,
        "fixture",
        "/tmp",
        10,
        None,
        None,
        None,
        Some(run_store),
        None,
        None,
        None,
        Some(AgentId::new()),
        None,
        built,
        session,
        None,
        None,
    )
    .await
    .expect("sync child must settle");
    assert!(matches!(outcome, ChildRunOutcome::Settled), "{outcome:?}");

    let result = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match observe.recv().await {
                Ok(crate::agent::events_v2::ObserveEvent::SubagentStop { result, .. }) => {
                    return result;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => panic!("observe channel closed before SubagentStop: {error}"),
            }
        }
    })
    .await
    .expect("SubagentStop event");
    assert!(
        result.chars().count() > 500,
        "SubagentStop.result 必须保留超过 500 字符的完整输出，实际 {} 字符",
        result.chars().count()
    );
    assert!(
        result.ends_with(MARKER),
        "SubagentStop.result 必须保留末尾标记，实际结尾: {}",
        &result[result.len().saturating_sub(80)..]
    );
}

include!("run_sync_handoff_lifecycle_test.rs");

#[path = "run_sync_cancellation_test.rs"]
mod cancellation_tests;
