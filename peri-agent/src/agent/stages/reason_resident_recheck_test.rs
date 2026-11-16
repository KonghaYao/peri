// Responsibility-specific cases share the original test namespace and fixtures.

struct FinalAnswerLlm;

#[async_trait]
impl crate::agent::react::ReactLLM for FinalAnswerLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        Ok(crate::agent::react::Reasoning::with_answer(
            "post-check reasoning",
            "done",
        ))
    }

    fn model_name(&self) -> String {
        "resident-reason-fixture".into()
    }
}

struct AppendHumanBeforeModel;

#[async_trait]
impl Middleware for AppendHumanBeforeModel {
    fn name(&self) -> &str {
        "AppendHumanBeforeModel"
    }

    async fn before_model(
        &self,
        state: &mut dyn hook_state::BeforeModelState,
    ) -> crate::error::AgentResult<()> {
        state.add_message(BaseMessage::human("hooks injected input"));
        Ok(())
    }
}

fn reason_context(
    bound: &TestSession,
    transcript: MessageTranscript,
    limit: u64,
    compact_model: Arc<RecordingModel>,
    hook: bool,
) -> StageContext {
    let (_session, mut context) =
        build_context(bound, transcript, resident_config(limit), compact_model);
    context.runtime.llm = Arc::new(FinalAnswerLlm);
    if hook {
        let mut chain = crate::middleware::MiddlewareChain::new();
        chain.add(Box::new(AppendHumanBeforeModel));
        context.runtime.middleware_chain = Arc::new(chain);
    }
    context
}

/// hooks 后的 canonical 变化是独立的驻留补检证据：不要求投影 token 增长。
#[tokio::test]
async fn reason_rechecks_resident_after_hooks_canonical_change() {
    let bound = TestSession::open().await;
    let compact_model = RecordingModel::new(false);
    let context = reason_context(
        &bound,
        build_transcript(&bound),
        1,
        compact_model.clone(),
        true,
    );

    crate::agent::stages::reason::run_reason(ReasonInput {
        context,
        has_tool_calls: false,
    })
    .await
    .expect("the run reason fixture result is required for this assertion");
    assert_eq!(
        compact_model.request_count(),
        1,
        "hooks 后新增 canonical 且驻留超限必须补检一次 Full"
    );
}

#[tokio::test]
async fn reason_skips_resident_recheck_without_canonical_change() {
    let bound = TestSession::open().await;
    let compact_model = RecordingModel::new(false);
    let context = reason_context(
        &bound,
        build_transcript(&bound),
        1,
        compact_model.clone(),
        false,
    );

    crate::agent::stages::reason::run_reason(ReasonInput {
        context,
        has_tool_calls: false,
    })
    .await
    .expect("the run reason fixture result is required for this assertion");
    assert_eq!(
        compact_model.request_count(),
        0,
        "canonical 未变化时不得仅因驻留超限补检"
    );
}

#[tokio::test]
async fn reason_skips_resident_recheck_below_limit() {
    let bound = TestSession::open().await;
    let compact_model = RecordingModel::new(false);
    let context = reason_context(
        &bound,
        build_transcript(&bound),
        u64::MAX,
        compact_model.clone(),
        true,
    );

    crate::agent::stages::reason::run_reason(ReasonInput {
        context,
        has_tool_calls: false,
    })
    .await
    .expect("the run reason fixture result is required for this assertion");
    assert_eq!(
        compact_model.request_count(),
        0,
        "hooks 改变 canonical 但未超限时不得补检 Full"
    );
}
