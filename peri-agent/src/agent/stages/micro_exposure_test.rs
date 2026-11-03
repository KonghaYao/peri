//! Real RCRA success/barrier evidence, using a durable session and an executable recovery tool.
use super::*;
use crate::session::{test_resources::TestSession, FrozenContext, Session};
use crate::tools::{
    BaseTool, ContextRetention, ToolContext, ToolExecutionEvidence, ToolExecutionStatus, ToolOutput,
};
use peri_acp_types::context_archive::ContextArchiveReadPort;

struct ReadFixture;
#[async_trait::async_trait]
impl BaseTool for ReadFixture {
    fn name(&self) -> &str {
        "ReadFixture"
    }
    fn description(&self) -> &str {
        "Read the immutable fixture"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::Recomputable
    }
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.invoke_output(input, ctx).await?.text)
    }
    async fn invoke_output(
        &self,
        _: serde_json::Value,
        _: ToolContext<'_>,
    ) -> Result<ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
        Ok(ToolOutput::with_execution(
            "canonical-read-data ".repeat(4000),
            ToolExecutionEvidence {
                status: ToolExecutionStatus::Completed,
                task_id: None,
                exit_code: None,
                output_ref: None,
                output_truncated: false,
            },
        ))
    }
}
struct RecoveryFixture;
#[async_trait::async_trait]
impl BaseTool for RecoveryFixture {
    fn name(&self) -> &str {
        "ReadContextArchive"
    }
    fn description(&self) -> &str {
        "Recover original history"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::ArchiveRead
    }
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let reader = crate::context_archive::ContextArchiveReader::new(
            ctx.session_resources.unwrap(),
            ctx.session_id.unwrap(),
        );
        Ok(serde_json::to_string(
            &reader.read(serde_json::from_value(input)?).await?,
        )?)
    }
}
struct Script {
    requests: Arc<parking_lot::Mutex<Vec<Vec<BaseMessage>>>>,
    fail_second: bool,
}
#[async_trait::async_trait]
impl ReactLLM for Script {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _: &[&dyn BaseTool],
        _: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        let step = self.requests.lock().len();
        self.requests.lock().push(messages.to_vec());
        if self.fail_second && step == 1 {
            return Err(crate::error::AgentError::LlmError(
                "injected response failure".into(),
            ));
        }
        let mut output = if step == 4 {
            crate::agent::react::Reasoning::with_answer("", "done")
        } else {
            crate::agent::react::Reasoning::with_tools(
                "",
                vec![crate::agent::react::ToolCall::new(
                    format!("read-{step}"),
                    "ReadFixture",
                    serde_json::json!({}),
                )],
            )
        };
        output.usage = Some(peri_model::TokenUsage {
            input_tokens: if step >= 2 { 76_000 } else { 1_000 },
            output_tokens: 1,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        Ok(output)
    }
}
async fn fixture(
    fail_second: bool,
) -> (
    TestSession,
    StageContext,
    Arc<parking_lot::Mutex<Vec<Vec<BaseMessage>>>>,
) {
    let bound = TestSession::open().await;
    let session = Session::new(
        Arc::from("/tmp/micro-exposure"),
        FrozenContext::builder().build(),
        Some(bound.thread_id.clone()),
    );
    {
        let transcript = session.transcript();
        let mut guard = transcript.write();
        *guard = std::mem::take(&mut *guard)
            .with_persistence(bound.resources(), bound.thread_id.clone());
        guard.append(BaseMessage::human("Keep the API unchanged."));
    }
    session.queue().push(crate::session::QueuedMessage::prompt(
        crate::session::MessageSource::UserInput,
        BaseMessage::human("Continue the task."),
    ));
    let requests = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([
        (
            "ReadFixture".into(),
            Arc::new(ReadFixture) as Arc<dyn BaseTool>,
        ),
        (
            "ReadContextArchive".into(),
            Arc::new(RecoveryFixture) as Arc<dyn BaseTool>,
        ),
    ])));
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(Script {
        requests: requests.clone(),
        fail_second,
    }))
    .with_tools(tools)
    .with_context_budget(crate::agent::token::ContextBudget::new(100_000))
    .with_compact_config(CompactConfig {
        micro_compact_stale_steps: 0,
        ..Default::default()
    })
    .build();
    (bound, context, requests)
}

#[tokio::test]
async fn successful_response_barrier_allows_only_previously_exposed_results() {
    let (bound, context, requests) = fixture(false).await;
    let outcome = run_react_loop(context.clone(), 5).await;
    assert!(matches!(outcome, LoopResult::Completed), "{outcome:?}");
    {
        let requests = requests.lock();
        assert_eq!(requests.len(), 5);
        let old = requests[3]
            .iter()
            .find(|m| matches!(m, BaseMessage::Tool {tool_call_id,..} if tool_call_id == "read-0"));
        assert!(
            old.is_none_or(|m| m.content().chars().count() < 70_000),
            "consumed old result should be projected"
        );
        let newest = requests[3]
            .iter()
            .find(|m| matches!(m, BaseMessage::Tool {tool_call_id,..} if tool_call_id == "read-2"))
            .unwrap();
        assert_eq!(
            newest.content(),
            format!(
                "{}\n[Execution status: completed]",
                "canonical-read-data ".repeat(4000)
            ),
            "fresh result has not crossed a successful response barrier"
        );
        assert!(requests[3]
            .iter()
            .any(|m| m.content().contains("ReadContextArchive")));
    }
    let snapshot = bound
        .resources()
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert!(snapshot
        .flags
        .values()
        .any(|flags| flags.projection.is_some()));
    assert!(snapshot
        .payloads
        .iter()
        .filter_map(|p| p.as_message())
        .filter(|m| matches!(m, BaseMessage::Tool { .. }))
        .all(|m| m.content()
            == format!(
                "{}\n[Execution status: completed]",
                "canonical-read-data ".repeat(4000)
            )));
}

#[tokio::test]
async fn failed_model_response_does_not_confirm_captured_result() {
    let (_bound, context, requests) = fixture(true).await;
    let _ = run_react_loop(context.clone(), 5).await;
    assert_eq!(requests.lock().len(), 2);
    let transcript = context.session.transcript.read();
    let exposed = context
        .compact
        .exposure
        .lock()
        .exposed_ids(&transcript)
        .unwrap();
    let result = transcript
        .entries()
        .iter()
        .filter_map(|e| e.as_message())
        .find(|m| matches!(m, BaseMessage::Tool { .. }))
        .unwrap();
    assert!(
        !exposed.contains(&result.id()),
        "failed response cannot authorize compaction"
    );
    assert!(!transcript.is_truncated(result.id()));
}
