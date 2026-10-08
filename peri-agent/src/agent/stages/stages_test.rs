//! 阶段输入输出与消息入队契约；按职责挂载循环行为测试。
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

/// Mock LLM：首轮返回 final_answer，无 tool_calls
struct FinalAnswerLLM {
    answer: &'static str,
}

#[async_trait::async_trait]
impl ReactLLM for FinalAnswerLLM {
    async fn generate_reasoning(
        &self,
        _messages: &[BaseMessage],
        _tools: &[&dyn crate::tools::BaseTool],
        _streaming: Option<crate::agent::react::StreamingContext>,
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
    let output = ReceiveOutput {
        consumed_count: 0,
        wake_up_count: 0,
        budget_deferred_count: 0,
        input_message_ids: Vec::new(),
        stop_requested: false,
    };
    assert_eq!(output.consumed_count, 0);
    assert_eq!(output.wake_up_count, 0);
}

#[test]
fn test_reason_input_output_contract() {
    let ctx = make_stage_context();
    let _input = ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    };
    let reasoning = crate::agent::react::Reasoning::with_answer("thinking", "answer");
    let output = ReasonOutput {
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
        messages_snapshot: std::sync::Arc::new(vec![]),
    };
    assert!(!output.reasoning.needs_tool_call());
    assert!(output.messages_snapshot.is_empty());
}

#[test]
fn test_act_input_output_contract() {
    let ctx = make_stage_context();
    let reasoning = crate::agent::react::Reasoning::with_answer("thinking", "done");
    let _input = ActInput {
        context: ctx.clone(),
        reasoning,
        catalog: ctx.runtime.tool_catalog.snapshot(),
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
    let content = transcript.entries()[0].message().content();
    assert_eq!(content, "hello user");
}

#[test]
fn test_append_messages_empty_prompt_skipped() {
    // keepgoing：空 Prompt（真实 payload 为 `MessageContent::text("")`，见
    // peri-tui submit_consumer handle_keepgoing_submit）驱动 loop 继续但不写入
    // transcript——用户没有输入新内容，历史中不应出现空 user 消息。
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 0, "空 Prompt 不应写入 transcript");
}

#[test]
fn test_append_messages_whitespace_prompt_kept() {
    // 空白文本不算空——与 peri-acp `is_keepgoing` 的 content-block 判空一致：
    // 按 content block 判空（`Blocks([Image])` 等纯附件消息不应被误判为空），
    // 而非按 text trim 判空；用户输入空格应正常写入。
    let ctx = make_stage_context();
    let msgs = vec![QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human(MessageContent::text("   ")),
    )];
    {
        let mut transcript = ctx.session.transcript.write();
        append_messages_to_transcript(&mut transcript, msgs);
    }
    let transcript = ctx.session.transcript.read();
    assert_eq!(transcript.len(), 1, "空白 Prompt 应正常写入 transcript");
}

#[test]
fn test_append_messages_info_kept_as_plain_message() {
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
    assert_eq!(transcript.entries()[0].message().content(), "system info");
}

#[test]
fn test_append_messages_defer_kept_as_plain_message() {
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
    assert_eq!(
        transcript.entries()[0].message().content(),
        "bg-result-payload"
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

    // transcript 应包含普通 Defer 内容，保持 legacy event/message 语义。
    let transcript = ctx.session.transcript.read();
    let combined: String = transcript
        .visible_messages()
        .iter()
        .map(|m| m.content().to_string())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(
        combined.contains("bg-result-payload"),
        "Defer 内容应在 transcript 中, got: {}",
        combined
    );
}

#[path = "loop_iteration_test.rs"]
mod loop_iteration_tests;

#[path = "loop_lifecycle_test.rs"]
mod loop_lifecycle_tests;

#[path = "input_hooks_test.rs"]
mod input_hooks_tests;

#[path = "startup_gate_test.rs"]
mod startup_gate_tests;

#[path = "compaction_loop_test.rs"]
mod compaction_loop_tests;
