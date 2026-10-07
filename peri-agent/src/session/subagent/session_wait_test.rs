use super::*;
use crate::agent::async_tasks::BgTaskKind;
use crate::agent::events::BackgroundTaskResult;
use crate::agent::react::{Reasoning, StreamingContext};
use crate::agent::stages::{run_react_loop, LoopResult};
use crate::session::test_resources::TestSession;
use std::sync::atomic::{AtomicUsize, Ordering};

struct ResultLlm(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl ReactLLM for ResultLlm {
    async fn generate_reasoning(
        &self,
        messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        if messages
            .iter()
            .any(|message| message.content().contains("child-result"))
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Reasoning::with_answer("", "processed"))
        } else {
            assert!(messages
                .iter()
                .any(|message| message.content().contains("initial child work")));
            Ok(Reasoning::with_answer("", "initial work done"))
        }
    }
}

async fn run_child(child: Arc<Session>, calls: Arc<AtomicUsize>) {
    let child_id = child.store().thread_id.clone().unwrap();
    let child_owner = child.clone();
    let built = build_v2_subagent_context(
        Some(child),
        Box::new(ResultLlm(calls)),
        MiddlewareChain::new(),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        CancellationToken::new(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&child_id)),
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(built.context, 20),
    )
    .await
    .unwrap();
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    crate::session::subagent::flush_session_history(&child_owner)
        .await
        .unwrap();
}

async fn terminal_after_optional_first_attempt(first_attempt: bool) {
    let bound = TestSession::open().await;
    let child_id = bound.thread_id();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    *child.transcript().write() = crate::session::MessageTranscript::new()
        .with_persistence(bound.resources(), child_id.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    if first_attempt {
        child.queue().push(crate::session::QueuedMessage::prompt(
            crate::session::MessageSource::UserInput,
            crate::messages::BaseMessage::human("initial child work"),
        ));
        run_child(child.clone(), calls.clone()).await;
    }
    let callback = crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            child.queue().clone(),
        ),
    );
    let terminal = BackgroundTaskResult {
        task_id: "child-task".into(),
        agent_name: "fixture".into(),
        prompt_summary: "child work".into(),
        success: true,
        output: "child-result".into(),
        tool_calls_count: 0,
        duration_ms: 1,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while callback(&terminal, BgTaskKind::Agent).is_err() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    run_child(child.clone(), calls.clone()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let saved = bound
        .resources
        .load_session_history(&child_id)
        .await
        .unwrap();
    assert!(saved.iter().any(|payload| match payload {
        peri_acp_types::store::PersistedPayload::Message(message) =>
            message.content().contains("processed"),
        _ => false,
    }));
}

#[tokio::test]
async fn child_processes_accepted_terminal_result_without_parent_loop() {
    terminal_after_optional_first_attempt(false).await;
}

#[tokio::test]
async fn child_terminal_result_after_finished_attempt_starts_fresh_current_run() {
    terminal_after_optional_first_attempt(true).await;
}
