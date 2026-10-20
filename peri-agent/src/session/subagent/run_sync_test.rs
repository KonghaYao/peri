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
        Some(store),
        None,
    );
    owner.set_writer(session.transcript().read().persist_tx_handle());
    manager.register_child_owner(owner.clone()).unwrap();
    session.set_subagent_host(SubagentHost {
        task_manager: Some(manager),
        child_owner: Some(owner.clone()),
        ..Default::default()
    });
    (session, owner)
}

struct AnswerLlm(&'static str);

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

fn shell_task(manager: &TaskManager, id: &str) {
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
    .unwrap();
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
    shell_task(&manager, "shell-task");
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
    assert!(manager.complete("shell-task", shell_result("shell-task")));
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

/// 截止后交接：父结果 failed、保留 child/task 引用，registry 接管受阻交付。
#[tokio::test(start_paused = true)]
async fn handoff_sync_child_returns_pending_and_registry_retries_delivery() {
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
    let attempts = Arc::new(AtomicUsize::new(0));
    let callback_attempts = attempts.clone();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = parking_lot::Mutex::new(Some(delivered_tx));
    let delivery: OnBgCompleteFn = Arc::new(move |result, _| {
        if callback_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
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
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    tokio::time::timeout(Duration::from_secs(2), async {
        while sync_state.sync_workers.len() != 0 {
            peri_time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("wrapper must release its session worker after registry takeover");
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
    parent_manager.begin_session_close();
    assert!(!parent_manager.wait_session_close().await);
    assert!(parent_manager.has_pending_delivery(&task_id));
}

/// 关闭失败必须保留诊断，不因同步 worker 结束而宣称成功。
#[tokio::test]
async fn explicit_close_reports_failed_sync_delivery() {
    let manager = Arc::new(TaskManager::new());
    let session = child(manager.clone());
    session
        .subagent_host()
        .unwrap()
        .close_state
        .record_sync_delivery_failure("sync-broken", "callback rejected");
    let error = super::super::close::close_subagent_session_scope(session)
        .await
        .unwrap_err();
    assert!(error.contains("callback rejected"), "{error}");
}
