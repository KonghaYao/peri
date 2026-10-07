use super::*;
use crate::agent::async_tasks::{durable_task_terminal_delivery, BgTaskKind};
use crate::agent::events::BackgroundTaskResult;
use crate::agent::react::{Reasoning, StreamingContext};
use crate::agent::stages::{run_react_loop, LoopResult};
use crate::session::subagent::SubagentHost;
use crate::session::test_resources::{
    mock::{admission::FixtureAdmission, work::bind_fixture_task},
    TestSession,
};
use peri_acp_types::session_resources::work::WorkQuery;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone)]
struct ResultLlm(Arc<AtomicUsize>);

impl ResultLlm {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        let _ = cancellation;
        use crate::session::test_resources::mock::model as fixture;
        let messages = fixture::base_messages(&request);
        let text = if messages
            .iter()
            .any(|message| message.content().contains("child-result"))
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            "processed"
        } else {
            assert!(messages
                .iter()
                .any(|message| message.content().contains("initial child work")));
            "initial work done"
        };
        fixture::text_events(text)
    }
}
crate::fixture_model_impl!(ResultLlm);

async fn run_child(child: Arc<Session>, calls: Arc<AtomicUsize>) {
    let child_id = child.store().thread_id.clone().unwrap();
    let child_owner = child.clone();
    let mut built = build_v2_subagent_context(
        Some(child),
        Box::new(crate::agent::model_bridge::AgentModelBridge::new(
            std::sync::Arc::new(ResultLlm(calls)),
        )),
        Arc::new(MiddlewareChain::new()),
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
    built.context.recipient_lifecycle = Some(1);
    let turn = built.context.session.turn.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_react_loop(built.context, 20),
    )
    .await
    .unwrap();
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    let transcript = child_owner.transcript();
    let owned = std::mem::take(&mut *transcript.write());
    owned.flush_persistence().await.unwrap();
    *transcript.write() = owned;
    let admission = turn.work_admission().unwrap().clone();
    use peri_acp_types::execution_admission::{
        AttemptStoppedProof, SettlementOutcome, SettlementRequest,
    };
    let outcome = child_owner
        .subagent_host()
        .unwrap()
        .execution_admission_port
        .as_ref()
        .unwrap()
        .settle(SettlementRequest {
            admission: admission.clone(),
            proof: AttemptStoppedProof::AttemptStopped {
                instance_id: admission.instance_id.clone(),
                generation_id: admission.generation_id.clone(),
                execution: admission.execution.clone(),
                evidence_id: format!("fixture-owner-stopped:{}", admission.admission_id),
            },
        })
        .await
        .unwrap();
    assert!(
        matches!(outcome, SettlementOutcome::Applied { receipt } if receipt.admission == admission)
    );
}

async fn terminal_after_optional_first_attempt(first_attempt: bool) {
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let bound = TestSession::open().await;
    let child_id = bound.thread_id();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    *child.transcript().write() = crate::session::MessageTranscript::new()
        .with_persistence(bound.resources(), child_id.clone());
    child.set_subagent_host(SubagentHost {
        session_resources: Some(bound.resources()),
        execution_admission_port: Some(Arc::new(FixtureAdmission(bound.resources()))),
        ..Default::default()
    });
    let calls = Arc::new(AtomicUsize::new(0));
    if first_attempt {
        child.queue().push(crate::session::QueuedMessage::prompt(
            crate::session::MessageSource::UserInput,
            crate::messages::BaseMessage::human("initial child work"),
        ));
        run_child(child.clone(), calls.clone()).await;
        let finished = bound
            .resources
            .load_session_work(&WorkQuery {
                session_id: child_id.clone(),
                limit: 1,
            })
            .await
            .unwrap();
        assert!(finished.candidates.is_empty());
        assert!(finished.control.attempt.is_none());
        assert_eq!(finished.state.admissions.len(), 1);
    }
    bind_fixture_task(bound.resources(), &child_id, 1, "child-task").await;
    let callback =
        crate::session::bg_complete::durable_bg_complete_callback(durable_task_terminal_delivery(
            bound.resources(),
            child_id.clone(),
            1,
            child.queue().clone(),
        ));
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
    let accepted = bound
        .resources
        .load_session_work(&WorkQuery {
            session_id: child_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(accepted.candidates.len(), 1);
    assert_eq!(accepted.state.task_bindings.len(), 1);
    run_child(child.clone(), calls.clone()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(parent.queue().is_empty());
    assert!(parent.transcript().read().visible_messages().is_empty());
    assert!(child
        .transcript()
        .read()
        .visible_messages()
        .iter()
        .any(|message| message.content().contains("processed")));
    let settled = bound
        .resources
        .load_session_work(&WorkQuery {
            session_id: child_id,
            limit: 1,
        })
        .await
        .unwrap();
    assert!(settled.candidates.is_empty());
    assert!(settled.control.attempt.is_none());
    assert_eq!(
        settled.state.admissions.len(),
        if first_attempt { 2 } else { 1 }
    );
    assert!(settled
        .state
        .admissions
        .values()
        .all(|admission| admission.settled_receipt.is_some()));
}

#[tokio::test]
async fn child_processes_accepted_terminal_result_without_parent_loop() {
    terminal_after_optional_first_attempt(false).await;
}

#[tokio::test]
async fn child_terminal_result_after_finished_attempt_requires_new_sdk_admission() {
    terminal_after_optional_first_attempt(true).await;
}
