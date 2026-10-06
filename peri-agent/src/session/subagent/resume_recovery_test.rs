use super::super::*;
use peri_acp_types::session_resources::{work::*, SessionResources};

struct UnknownToolReasonLlm;

#[async_trait::async_trait]
impl crate::agent::react::ReactLLM for UnknownToolReasonLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        Ok(crate::agent::react::Reasoning::with_tools(
            "completed model response",
            vec![crate::agent::react::ToolCall::new(
                "unknown-call",
                "missing-tool",
                serde_json::json!({}),
            )],
        ))
    }
}

async fn failed_child() -> (Arc<MockSessionResources>, String, WorkInspection) {
    let store = MockSessionResources::new();
    let child_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &child_id, None).await;
    store
        .append_messages(
            &child_id,
            &[BaseMessage::tool_result(
                "completed-read",
                "retained read result",
            )],
        )
        .await
        .unwrap();
    let config = resume_config_with(
        store.clone(),
        child_id.clone(),
        Box::new(UnknownToolReasonLlm),
        SubagentRunMode::Sync,
        None,
        None,
    );
    let _failure = AdmittedSessionFactory::resume_subagent(None, config)
        .await
        .err()
        .expect("reason fixture must fail");
    let snapshot = store
        .inspect_work(&WorkQuery::new(&child_id, WorkSelector::ActiveProcessing))
        .await
        .unwrap();
    let WorkPage::Processings(records) = &snapshot.page else {
        panic!("expected active processing page")
    };
    assert!(records
        .iter()
        .any(|processing| processing.stage == WorkStage::ReasonInFlight
            && processing.response.is_none()));
    assert!(snapshot.control.attempt.is_none());
    (store, child_id, snapshot)
}

#[tokio::test]
async fn explicit_resume_supersedes_failed_reason_without_replaying_or_erasing_it() {
    let (store, child_id, before) = failed_child().await;
    let WorkPage::Processings(processings) = &before.page else {
        panic!("expected active processing page")
    };
    let original = processings
        .iter()
        .find(|processing| processing.stage == WorkStage::ReasonInFlight)
        .unwrap();
    let evidence_query = EvidenceQuery {
        session_id: child_id.clone(),
        reference: original.request.clone().unwrap(),
    };
    let original_evidence = store.read_evidence(&evidence_query).await.unwrap();
    original_evidence.validate().unwrap();
    let llm = RecordingLLM::new();
    let calls = llm.received.clone();
    let mut config = resume_config_with(
        store.clone(),
        child_id.clone(),
        Box::new(llm),
        SubagentRunMode::Sync,
        None,
        None,
    );
    config.prompt = Some("Continue the task and list the directory".into());
    let resumed = AdmittedSessionFactory::resume_subagent(None, config)
        .await
        .expect("explicit authorized input must be admitted after failed Reason");
    assert_eq!(resumed.child_thread_id, child_id);
    let abandoned =
        crate::session::work_access::processing(store.as_ref(), &child_id, &original.processing_id)
            .await
            .unwrap();
    assert_eq!(abandoned.stage, WorkStage::Abandoned);
    assert_eq!(abandoned.request_id, original.request_id);
    let retained_evidence = store.read_evidence(&evidence_query).await.unwrap();
    retained_evidence.validate().unwrap();
    assert_eq!(retained_evidence, original_evidence);
    let after = store
        .inspect_work(&WorkQuery::new(&child_id, WorkSelector::ActiveProcessing))
        .await
        .unwrap();
    let WorkPage::Processings(records) = after.page else {
        panic!("expected active processing page")
    };
    assert!(!records.iter().any(|processing| matches!(
        processing.stage,
        WorkStage::ReasonInFlight | WorkStage::Blocked
    )));
    let messages = calls.read();
    assert_eq!(messages.len(), 1);
    assert!(messages[0]
        .iter()
        .any(|message| message.content().contains("retained read result")));
    assert!(messages[0].iter().any(|message| message
        .content()
        .contains("Continue the task and list the directory")));
}

#[tokio::test]
async fn implicit_resume_refuses_failed_reason_without_publishing_an_orphan_input() {
    let (store, child_id, before) = failed_child().await;
    let llm = RecordingLLM::new();
    let calls = llm.received.clone();
    let config = resume_config_with(
        store.clone(),
        child_id.clone(),
        Box::new(llm),
        SubagentRunMode::Sync,
        None,
        None,
    );
    let error = AdmittedSessionFactory::resume_subagent(None, config)
        .await
        .err()
        .expect("implicit resume must not regenerate an unresolved Reason request");
    assert_eq!(
        error
            .downcast_ref::<crate::tools::EffectiveToolError>()
            .unwrap()
            .code,
        crate::tools::EffectiveToolErrorCode::ApplicationFailed
    );
    let after = store
        .inspect_work(&WorkQuery::new(&child_id, WorkSelector::ActiveProcessing))
        .await
        .unwrap();
    assert_eq!(after.page, before.page);
    assert_eq!(after.head.next_delivery_seq, before.head.next_delivery_seq);
    assert!(calls.read().is_empty());
}

#[tokio::test]
async fn completed_child_failure_returns_a_tool_error_without_aborting_parent_dispatch() {
    let (store, child_id, _) = failed_child().await;
    let mut config = resume_config_with(
        store,
        child_id.clone(),
        Box::new(UnknownToolReasonLlm),
        SubagentRunMode::Sync,
        None,
        None,
    );
    config.prompt = Some("Continue with available tools".into());
    let outcome = super::dispatch_resume_fixture(config, &CancellationToken::new())
        .await
        .expect("confirmed child failure must not abort parent dispatch");
    assert_eq!(outcome.results.len(), 1);
    let result = &outcome.results[0].1;
    assert!(result.is_error);
    assert_eq!(
        result.effective_error_code,
        Some(crate::tools::EffectiveToolErrorCode::ApplicationFailed)
    );
    assert!(result.output.contains(&child_id));
    assert!(result.output.contains("missing-tool"));
}
