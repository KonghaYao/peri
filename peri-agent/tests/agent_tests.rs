use async_trait::async_trait;
use peri_agent::{
    messages::{BaseMessage, ContentBlock, MessageContent},
    prelude::*,
};

// ── 辅助工具（实现 BaseTool trait） ────────────────────────────────────────────

/// 提供 echo 工具的中间件（用于测试 collect_tools 自动注册流程）
struct EchoMiddleware;

#[async_trait]
impl<S: peri_agent::agent::state::State> peri_agent::middleware::r#trait::Middleware<S>
    for EchoMiddleware
{
    fn name(&self) -> &str {
        "EchoMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(EchoTool)]
    }
}

/// 覆盖 echo 工具的中间件（返回不同输出，用于测试优先级）
struct OverrideEchoTool;

#[async_trait]
impl BaseTool for OverrideEchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Override echo"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": { "text": { "type": "string" } } })
    }
    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(format!(
            "override: {}",
            input["text"].as_str().unwrap_or("")
        ))
    }
}

struct EchoTool;

#[async_trait]
impl BaseTool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echoes the input back"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": { "text": { "type": "string" } } })
    }
    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(format!("echo: {}", input["text"].as_str().unwrap_or("")))
    }
}

struct FailingTool;

#[async_trait]
impl BaseTool for FailingTool {
    fn name(&self) -> &str {
        "fail"
    }
    fn description(&self) -> &str {
        "Always fails"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err("intentional failure".into())
    }
}

// ── 测试 ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_agent_simple_answer() {
    let agent = ReActAgent::new(MockLLM::always_answer("simple answer"));
    let mut state = AgentState::new("/test");

    let output = agent
        .execute(AgentInput::text("hello"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.text, "simple answer");
    assert_eq!(output.steps, 1);
    assert!(output.tool_calls.is_empty());
}

#[tokio::test]
async fn test_agent_tool_call_then_answer() {
    let llm = MockLLM::tool_then_answer(
        "echo",
        serde_json::json!({ "text": "hello world" }),
        "The echo said: hello world",
    );

    let agent = ReActAgent::new(llm).register_tool(Box::new(EchoTool));
    let mut state = AgentState::new("/test");

    let output = agent
        .execute(AgentInput::text("echo something"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.text, "The echo said: hello world");
    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].0.name, "echo");
    assert_eq!(output.tool_calls[0].1.output, "echo: hello world");
}

#[tokio::test]
async fn test_agent_tool_not_found() {
    let llm = MockLLM::tool_then_answer("nonexistent_tool", serde_json::json!({}), "done");
    let agent = ReActAgent::new(llm);
    let mut state = AgentState::new("/test");

    let output = agent
        .execute(AgentInput::text("use missing tool"), &mut state, None)
        .await
        .unwrap();

    // ToolNotFound 现在作为 ToolResult::error 返回，Agent 继续运行直到 LLM 给出最终回答
    assert_eq!(output.tool_calls.len(), 1);
    assert!(
        output.tool_calls[0].1.is_error,
        "ToolNotFound 应产生错误结果"
    );
    assert!(output.tool_calls[0].1.output.contains("not found"));
}

#[tokio::test]
async fn test_agent_failing_tool_is_recorded() {
    let llm = MockLLM::tool_then_answer("fail", serde_json::json!({}), "got error but continuing");
    let agent = ReActAgent::new(llm).register_tool(Box::new(FailingTool));
    let mut state = AgentState::new("/test");

    let output = agent
        .execute(AgentInput::text("try failing tool"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.tool_calls.len(), 1);
    assert!(output.tool_calls[0].1.is_error);
}

#[tokio::test]
async fn test_agent_max_iterations() {
    let calls: Vec<Reasoning> = (0..20)
        .map(|_| {
            Reasoning::with_tools(
                "still thinking",
                vec![ToolCall::new("c", "echo", serde_json::json!({"text":"hi"}))],
            )
        })
        .collect();

    let agent = ReActAgent::new(MockLLM::new(calls))
        .max_iterations(3)
        .register_tool(Box::new(EchoTool));
    let mut state = AgentState::new("/test");

    let result = agent
        .execute(AgentInput::text("loop forever"), &mut state, None)
        .await;
    assert!(matches!(result, Err(AgentError::MaxIterationsExceeded(3))));
}

// ── 中间件工具自注册测试 ──────────────────────────────────────────────────────

/// 验证通过 add_middleware 自动注册工具（无需手动 register_tool）
#[tokio::test]
async fn test_middleware_auto_registers_tools() {
    let llm = MockLLM::tool_then_answer(
        "echo",
        serde_json::json!({ "text": "from middleware" }),
        "got: echo: from middleware",
    );

    // 只通过 add_middleware 注册中间件，不手动调用 register_tool
    let agent = ReActAgent::new(llm).add_middleware(Box::new(EchoMiddleware));
    let mut state = AgentState::new("/test");

    let output = agent
        .execute(AgentInput::text("use echo"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.text, "got: echo: from middleware");
    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].1.output, "echo: from middleware");
    assert!(!output.tool_calls[0].1.is_error);
}

/// 验证手动 register_tool 的同名工具优先于中间件提供的工具
#[tokio::test]
async fn test_manual_tool_overrides_middleware_tool() {
    let llm = MockLLM::tool_then_answer(
        "echo",
        serde_json::json!({ "text": "priority test" }),
        "done",
    );

    // EchoMiddleware 提供 echo 工具，但 register_tool(OverrideEchoTool) 应优先
    let agent = ReActAgent::new(llm)
        .add_middleware(Box::new(EchoMiddleware))
        .register_tool(Box::new(OverrideEchoTool));
    let mut state = AgentState::new("/test");

    let output = agent
        .execute(AgentInput::text("echo with override"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.tool_calls.len(), 1);
    // 应使用 OverrideEchoTool 的输出，而非 EchoTool 的输出
    assert_eq!(output.tool_calls[0].1.output, "override: priority test");
}

#[tokio::test]
async fn test_state_messages_grow() {
    let agent = ReActAgent::new(MockLLM::always_answer("ok"));
    let mut state = AgentState::new("/test");

    assert_eq!(state.messages().len(), 0);
    agent
        .execute(AgentInput::text("hello"), &mut state, None)
        .await
        .unwrap();

    // user message + assistant answer
    assert_eq!(state.messages().len(), 2);
}

// ── source_message / Reasoning block 透传测试 ─────────────────────────────────

/// 验证当 Reasoning 携带含 Reasoning block 的 source_message 时，
/// executor 会把该消息原样存入 state（而不是重新构造纯文本消息）
#[tokio::test]
async fn test_source_message_stored_in_state_for_answer() {
    // 构造含 Reasoning block 的 source_message
    let source = BaseMessage::ai(MessageContent::Blocks(vec![
        ContentBlock::reasoning("这是思考过程，共100字"),
        ContentBlock::text("最终答案"),
    ]));
    let mut r = Reasoning::with_answer("思考摘要", "最终答案");
    r.source_message = Some(source);

    let agent = ReActAgent::new(MockLLM::new(vec![r]));
    let mut state = AgentState::new("/test");

    let output = agent
        .execute(AgentInput::text("请回答"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(output.text, "最终答案");

    // state 中存的 AI 消息应含 Reasoning block
    let ai_msg = state
        .messages()
        .iter()
        .find(|m| matches!(m, BaseMessage::Ai { .. }))
        .expect("should have AI message");
    let blocks = ai_msg.content_blocks();
    assert_eq!(blocks.len(), 2, "should have 2 blocks: Reasoning + Text");
    assert!(
        matches!(blocks[0], ContentBlock::Reasoning { .. }),
        "first block should be Reasoning"
    );
    assert_eq!(blocks[0].as_reasoning(), Some("这是思考过程，共100字"));
    assert_eq!(blocks[1].as_text(), Some("最终答案"));
}

/// 验证工具调用场景下 source_message 同样被正确存入 state
#[tokio::test]
async fn test_source_message_stored_in_state_for_tool_call() {
    use peri_agent::messages::ToolCallRequest;

    let source = BaseMessage::ai_with_tool_calls(
        MessageContent::Blocks(vec![
            ContentBlock::reasoning("我需要用工具"),
            ContentBlock::text("调用 echo"),
        ]),
        vec![ToolCallRequest::new(
            "c1",
            "echo",
            serde_json::json!({"text": "hi"}),
        )],
    );
    let mut r = Reasoning::with_tools(
        "我需要用工具",
        vec![ToolCall::new(
            "c1",
            "echo",
            serde_json::json!({"text": "hi"}),
        )],
    );
    r.source_message = Some(source);

    let answer = Reasoning::with_answer("", "done");

    let agent = ReActAgent::new(MockLLM::new(vec![r, answer])).register_tool(Box::new(EchoTool));
    let mut state = AgentState::new("/test");

    agent
        .execute(AgentInput::text("call echo"), &mut state, None)
        .await
        .unwrap();

    // 找第一条 AI 消息（工具调用步骤）
    let ai_with_tool = state
        .messages()
        .iter()
        .find(|m| matches!(m, BaseMessage::Ai { tool_calls, .. } if !tool_calls.is_empty()))
        .expect("should have AI message with tool calls");
    let blocks = ai_with_tool.content_blocks();
    let has_reasoning = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Reasoning { .. }));
    assert!(
        has_reasoning,
        "tool call AI message should retain Reasoning block"
    );
}

// ── 中断（Cancellation）测试 ──────────────────────────────────────────────────

/// 验证传入已取消的 token 时，execute 立即返回 AgentError::Interrupted
#[tokio::test]
async fn test_cancel_before_execute_returns_interrupted() {
    use tokio_util::sync::CancellationToken;

    let token = CancellationToken::new();
    token.cancel(); // 提前取消

    let agent = ReActAgent::new(MockLLM::always_answer("should not reach"));
    let mut state = AgentState::new("/test");

    let result = agent
        .execute(AgentInput::text("hi"), &mut state, Some(token))
        .await;
    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "pre-cancelled token should give Interrupted, got: {:?}",
        result
    );
}

/// 验证正常执行时传 None cancel token 不影响结果
#[tokio::test]
async fn test_no_cancel_token_runs_normally() {
    let agent = ReActAgent::new(MockLLM::always_answer("no cancel"));
    let mut state = AgentState::new("/test");

    let output = agent
        .execute(AgentInput::text("hi"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(output.text, "no cancel");
}
