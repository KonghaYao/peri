//! 从 mod.rs 分离的测试模块
use super::*;
use crate::messages::MessageContent;
use crate::session::queue::MessageSource;
use crate::session::store::FrozenContext;
use crate::session::Session;

/// 构造测试用 StageContext
fn make_stage_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder()
        .system_prompt("You are a test agent.")
        .build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

// ── 类型契约测试 ──

#[test]
fn test_compact_input_output_contract() {
    let ctx = make_stage_context();
    let input = CompactInput {
        context: ctx,
        has_tool_calls: false,
    };
    assert!(!input.has_tool_calls);

    let output = CompactOutput { compacted: false };
    assert!(!output.compacted);
}

#[test]
fn test_receive_input_output_contract() {
    let ctx = make_stage_context();
    let _input = ReceiveInput { context: ctx };
    let output = ReceiveOutput { consumed_count: 0 };
    assert_eq!(output.consumed_count, 0);
}

#[test]
fn test_reason_input_output_contract() {
    let ctx = make_stage_context();
    let _input = ReasonInput {
        context: ctx,
        has_tool_calls: false,
    };
    let reasoning = crate::agent::react::Reasoning::with_answer("thinking", "answer");
    let output = ReasonOutput {
        reasoning,
        messages_snapshot: vec![],
    };
    assert!(!output.reasoning.needs_tool_call());
    assert!(output.messages_snapshot.is_empty());
}

#[test]
fn test_act_input_output_contract() {
    let ctx = make_stage_context();
    let reasoning = crate::agent::react::Reasoning::with_answer("thinking", "done");
    let _input = ActInput {
        context: ctx,
        reasoning,
    };

    let output_with_tools = ActOutput {
        has_tool_calls: true,
        final_answer: None,
    };
    assert!(output_with_tools.has_tool_calls);
    assert!(output_with_tools.final_answer.is_none());

    let output_no_tools = ActOutput {
        has_tool_calls: false,
        final_answer: Some("done".to_string()),
    };
    assert!(!output_no_tools.has_tool_calls);
    assert_eq!(output_no_tools.final_answer.as_deref(), Some("done"));
}

#[test]
fn test_stage_context_construction() {
    let ctx = make_stage_context();
    assert_eq!(&*ctx.session.turn.cwd, "/tmp/test");
    assert_eq!(ctx.session.turn.current_step(), 0);
    assert!(ctx.session.queue.is_empty());
    assert!(ctx.session.transcript.read().is_empty());
}

#[test]
fn test_stage_context_builder_default() {
    // builder 不传 llm 时，自动 fallback 到 NullReactLLM
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
    assert_eq!(ctx.runtime.llm.model_name(), "null");
}

// ── e2e 集成测试（验证完整 v2 ReAct 循环）──

/// MockLLM：首轮返回 final_answer，无 tool_calls
struct FinalAnswerLLM {
    answer: &'static str,
}
#[async_trait::async_trait]
impl ReactLLM for FinalAnswerLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::llm::types::StreamingContext>,
    ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
        Ok(crate::agent::react::Reasoning::with_answer(
            "thinking",
            self.answer,
        ))
    }
    fn model_name(&self) -> String {
        "mock-final-answer".to_string()
    }
}

#[tokio::test]
async fn test_e2e_final_answer_no_tools() {
    // e2e：推入 Prompt → run_react_loop → 直接 final_answer → Completed
    let cwd: Arc<str> = Arc::from("/tmp/e2e");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLLM {
            answer: "task completed",
        }))
        .build();

    // 推入用户输入
    ctx.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("do the task")),
    ));

    let result = run_react_loop(ctx.clone(), 10).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "expected Completed, got {:?}",
        result
    );

    // transcript 应包含：[user_prompt, ai_final_answer]
    let transcript = ctx.session.transcript.read();
    let visible: Vec<_> = transcript.visible_messages().into_iter().collect();
    assert_eq!(
        visible.len(),
        2,
        "expected 2 messages (user + ai), got {}",
        visible.len()
    );
    assert!(matches!(visible[0], BaseMessage::Human { .. }));
    assert!(matches!(visible[1], BaseMessage::Ai { .. }));
}

#[tokio::test]
async fn test_e2e_cancel_before_loop() {
    // e2e：cancel_token 在 run_react_loop 之前触发 → Interrupted
    let cwd: Arc<str> = Arc::from("/tmp/e2e-cancel");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLLM {
            answer: "should not reach",
        }))
        .build();

    // 立即 cancel
    ctx.session.turn.cancel_token.cancel();

    let result = run_react_loop(ctx, 10).await;
    assert!(
        matches!(result, LoopResult::Interrupted),
        "expected Interrupted, got {:?}",
        result
    );
}

#[tokio::test]
async fn test_e2e_empty_queue_completes_immediately() {
    // e2e：无 Prompt 推入 → Receive 阶段 consumed=0 → Completed
    let cwd: Arc<str> = Arc::from("/tmp/e2e-empty");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLLM { answer: "answer" }))
        .build();

    // 不推入 Prompt，直接跑循环（首轮 Receive consumed=0 → 直接退出）
    let result = run_react_loop(ctx.clone(), 10).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "expected Completed, got {:?}",
        result
    );

    // RCRA：空队列立即退出，不会进入 Reason/Act，transcript 为空
    let transcript = ctx.session.transcript.read();
    assert!(
        transcript.is_empty(),
        "expected empty transcript on immediate exit"
    );
}

// ── append_messages_to_transcript helper 测试 ──

#[test]
fn test_append_messages_prompt_kept_as_is() {
    // Prompt 消息应原样 append（用户输入不包裹 reminder）
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("hello user")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let content = transcript.entries()[0].message.content();
    assert_eq!(content, "hello user");
}

#[test]
fn test_append_messages_info_wrapped_in_reminder() {
    // Info 消息应用 <system-reminder> 包裹
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::info(
        MessageSource::SystemInjected,
        BaseMessage::human(MessageContent::text("system info")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let content = transcript.entries()[0].message.content();
    assert!(content.contains("<system-reminder>"));
    assert!(content.contains("system info"));
}

#[test]
fn test_append_messages_defer_wrapped_in_reminder() {
    // Defer 消息（bg_results / WorkflowComplete）应用 <system-reminder> 包裹
    // —— 这是本次修复的关键断言：mod.rs:520-528 把 awakened_messages 写入
    // transcript 时，Defer 走 reminder 包裹路径（与 Info 一致）。
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human(MessageContent::text("bg-result-payload")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1);
    let content = transcript.entries()[0].message.content();
    assert!(
        content.contains("<system-reminder>"),
        "Defer 应被 reminder 包裹, got: {}",
        content
    );
    assert!(
        content.contains("bg-result-payload"),
        "Defer 内容应在 transcript 中, got: {}",
        content
    );
}

#[tokio::test]
async fn test_e2e_defer_consumed_in_receive() {
    // RCRA：push Defer → run_react_loop → 第一轮 Receive 消费 Defer（drain_all）
    // → Compact → Reason → Act → Receive（空→退出）→ Completed。
    //
    // 迁移自原 test_e2e_defer_written_to_transcript_when_end_awakens，
    // 验证 Defer 在 RCRA 的 Receive 阶段被正确消费和写入 transcript。
    let cwd: Arc<str> = Arc::from("/tmp/rcra-defer");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(Arc::new(FinalAnswerLLM { answer: "ok" }))
        .build();

    ctx.session.queue.push(QueuedMessage::defer(
        MessageSource::SubAgentComplete,
        BaseMessage::human(MessageContent::text("bg-result-payload")),
    ));

    let result = run_react_loop(ctx.clone(), 5).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "expected Completed, got {:?}",
        result
    );

    // transcript 应包含 Defer 内容（reminder 包裹，由 Receive 阶段写入）
    let transcript = ctx.session.transcript.read();
    let combined: String = transcript
        .visible_messages()
        .iter()
        .map(|m| m.content().to_string())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(
        combined.contains("<system-reminder>"),
        "Defer 应被 reminder 包裹写入 transcript, got: {}",
        combined
    );
    assert!(
        combined.contains("bg-result-payload"),
        "Defer 内容应在 transcript 中, got: {}",
        combined
    );
}
