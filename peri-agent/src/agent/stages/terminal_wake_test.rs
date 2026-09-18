use super::*;
use crate::agent::async_tasks::{
    BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind, TaskManager,
};
use crate::agent::events::BackgroundTaskResult;
use crate::agent::react::{ReactLLM, Reasoning, StreamingContext};
use crate::messages::BaseMessage;
use crate::session::queue::MessageSource;
use crate::session::FrozenContext;
use std::sync::atomic::{AtomicBool as TestAtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

struct FinalAnswerLlm {
    calls: Arc<TestAtomicBool>,
}

#[async_trait::async_trait]
impl ReactLLM for FinalAnswerLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        self.calls.store(true, AtomicOrdering::SeqCst);
        Ok(Reasoning::with_answer("", "done"))
    }

    fn model_name(&self) -> String {
        "terminal-wake-test".to_string()
    }
}

fn make_task(id: &str) -> BackgroundTask {
    BackgroundTask {
        id: id.to_string(),
        agent_name: "test-agent".to_string(),
        prompt_summary: "terminal wake test".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(tokio::spawn(async {})),
        cancel_token: None,
        pid: None,
        output_preview: None,
        agent_inbox: None,
    }
}

#[tokio::test]
async fn test_idle_loop_exits_when_registry_completes_without_queue_message() {
    let manager = Arc::new(TaskManager::new());
    manager.register_with_kind(make_task("bg-1")).unwrap();
    let session = crate::session::Session::new(
        Arc::from("/tmp/terminal-wake-test"),
        FrozenContext::builder().build(),
        None,
    );
    let inbox = Arc::new(crate::agent::session::SessionInbox::new(Arc::new(
        session.queue().clone(),
    )));
    let suspended = Arc::new(TestAtomicBool::new(false));
    let turn = session.start_turn();
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_idle_inbox(inbox)
        .with_idle_should_wait({
            let manager = Arc::clone(&manager);
            Arc::new(move || manager.active_count() > 0)
        })
        .with_idle_registry(manager.registry().subscribe_activity())
        .with_idle_suspended_flag(Arc::clone(&suspended))
        .build();
    let task = tokio::spawn(run_react_loop(context, 0));

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !suspended.load(AtomicOrdering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("loop must reach idle wait before completion");

    assert!(manager.complete(
        "bg-1",
        BackgroundTaskResult {
            task_id: "bg-1".to_string(),
            agent_name: "test-agent".to_string(),
            prompt_summary: "terminal wake test".to_string(),
            success: true,
            output: "done".to_string(),
            tool_calls_count: 0,
            duration_ms: 1,
            child_thread_id: None,
            timed_out: false,
            subagent_failure: None,
            shell_output: None,
        },
    ));
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("registry completion must wake the idle loop")
        .expect("loop task must not panic");
    assert!(matches!(result, LoopResult::Completed));
    assert!(!suspended.load(AtomicOrdering::Acquire));
}

#[tokio::test]
async fn test_idle_loop_cancel_returns_interrupted_and_clears_suspended() {
    let manager = Arc::new(TaskManager::new());
    manager.register_with_kind(make_task("bg-cancel")).unwrap();
    let session = crate::session::Session::new(
        Arc::from("/tmp/terminal-wake-cancel-test"),
        FrozenContext::builder().build(),
        None,
    );
    let inbox = Arc::new(crate::agent::session::SessionInbox::new(Arc::new(
        session.queue().clone(),
    )));
    let suspended = Arc::new(TestAtomicBool::new(false));
    let turn = session.start_turn();
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_idle_inbox(inbox)
        .with_idle_should_wait({
            let manager = Arc::clone(&manager);
            Arc::new(move || manager.active_count() > 0)
        })
        .with_idle_registry(manager.registry().subscribe_activity())
        .with_idle_suspended_flag(Arc::clone(&suspended))
        .build();
    let cancel = context.session.turn.cancel_token.clone();
    let task = tokio::spawn(run_react_loop(context, 0));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !suspended.load(AtomicOrdering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("loop must reach idle wait before cancellation");

    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("cancel must wake idle loop")
        .expect("loop task must not panic");
    assert!(matches!(result, LoopResult::Interrupted));
    assert!(!suspended.load(AtomicOrdering::Acquire));
    manager.cancel("bg-cancel").unwrap();
}

#[tokio::test]
async fn test_idle_loop_consumes_queued_completion_before_terminal_registry_check() {
    let manager = Arc::new(TaskManager::new());
    manager.register_with_kind(make_task("bg-queued")).unwrap();
    let session = crate::session::Session::new(
        Arc::from("/tmp/terminal-wake-queued-test"),
        FrozenContext::builder().build(),
        None,
    );
    let inbox = Arc::new(crate::agent::session::SessionInbox::new(Arc::new(
        session.queue().clone(),
    )));
    let handle = inbox.handle();
    let suspended = Arc::new(TestAtomicBool::new(false));
    let injected = Arc::new(TestAtomicBool::new(false));
    let llm_called = Arc::new(TestAtomicBool::new(false));
    let turn = session.start_turn();
    let context = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLlm {
            calls: Arc::clone(&llm_called),
        }))
        .with_idle_inbox(inbox)
        .with_idle_should_wait({
            let manager = Arc::clone(&manager);
            let handle = handle.clone();
            let injected = Arc::clone(&injected);
            Arc::new(move || {
                if !injected.swap(true, AtomicOrdering::SeqCst) {
                    handle.push_defer(
                        MessageSource::SubAgentComplete,
                        BaseMessage::human("background done"),
                    );
                    assert!(manager.complete("bg-queued", completion_result("bg-queued")));
                }
                manager.active_count() > 0
            })
        })
        .with_idle_registry(manager.registry().subscribe_activity())
        .with_idle_suspended_flag(Arc::clone(&suspended))
        .build();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        run_react_loop(context, 1),
    )
    .await
    .expect("queued completion must not leave the loop waiting");
    assert!(matches!(result, LoopResult::Completed));
    assert!(llm_called.load(AtomicOrdering::SeqCst));
    assert!(!suspended.load(AtomicOrdering::Acquire));
}

fn completion_result(task_id: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.to_string(),
        agent_name: "test-agent".to_string(),
        prompt_summary: "terminal wake test".to_string(),
        success: true,
        output: "done".to_string(),
        tool_calls_count: 0,
        duration_ms: 1,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}
