use std::time::{Duration, Instant};

use super::*;
use crate::{
    agent::{
        react::{AgentInput, Reasoning},
        state::AgentState,
    },
    messages::BaseMessage,
    tools::BaseTool,
};

// ─── Mock LLM：第一步返回两个并发工具调用，第二步返回最终答案 ───────────

struct TwoToolCallLLM;

#[async_trait::async_trait]
impl ReactLLM for TwoToolCallLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<crate::llm::types::StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        let has_tool_result = messages
            .iter()
            .any(|m| matches!(m, BaseMessage::Tool { .. }));
        if !has_tool_result {
            Ok(Reasoning::with_tools(
                "need both tools",
                vec![
                    ToolCall::new("id1", "slow_tool_a", serde_json::json!({})),
                    ToolCall::new("id2", "slow_tool_b", serde_json::json!({})),
                ],
            ))
        } else {
            Ok(Reasoning::with_answer("done", "parallel ok"))
        }
    }
}

// ─── Mock 工具：sleep 100ms ────────────────────────────────────────────────

struct SlowTool {
    tool_name: &'static str,
}

#[async_trait::async_trait]
impl BaseTool for SlowTool {
    fn name(&self) -> &str {
        self.tool_name
    }
    fn description(&self) -> &str {
        "slow test tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(format!("{} done", self.tool_name))
    }
}

/// 验证两个各耗时 100ms 的工具并发执行，总耗时应 < 160ms（串行需 ≥ 200ms）
#[tokio::test]
async fn test_parallel_tool_execution() {
    let agent = ReActAgent::new(TwoToolCallLLM)
        .max_iterations(5)
        .register_tool(Box::new(SlowTool {
            tool_name: "slow_tool_a",
        }))
        .register_tool(Box::new(SlowTool {
            tool_name: "slow_tool_b",
        }));

    let mut state = AgentState::new("/tmp");
    let start = Instant::now();
    let output = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();
    let elapsed = start.elapsed();

    assert_eq!(output.text, "parallel ok");
    assert_eq!(output.tool_calls.len(), 2);
    assert!(
        elapsed < Duration::from_millis(160),
        "并行执行耗时 {:?}，应 < 160ms（串行需 ≥ 200ms）",
        elapsed
    );
}

/// 验证取消 token 触发时，工具以 error 收尾并返回 Interrupted
#[tokio::test]
async fn test_cancel_during_tool_execution() {
    struct HangingTool;
    #[async_trait::async_trait]
    impl BaseTool for HangingTool {
        fn name(&self) -> &str {
            "hanging_tool"
        }
        fn description(&self) -> &str {
            "hangs forever"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok("never".to_string())
        }
    }

    struct OneToolLLM;
    #[async_trait::async_trait]
    impl ReactLLM for OneToolLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            let has_tool = messages
                .iter()
                .any(|m| matches!(m, BaseMessage::Tool { .. }));
            if !has_tool {
                Ok(Reasoning::with_tools(
                    "call tool",
                    vec![ToolCall::new("id1", "hanging_tool", serde_json::json!({}))],
                ))
            } else {
                Ok(Reasoning::with_answer("done", "ok"))
            }
        }
    }

    let cancel = CancellationToken::new();
    let agent = ReActAgent::new(OneToolLLM)
        .max_iterations(5)
        .register_tool(Box::new(HangingTool));

    // 50ms 后触发取消
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, Some(cancel))
        .await;

    assert!(matches!(result, Err(AgentError::Interrupted)));
    // 工具 error 结果已写入 state（可用于断点续跑）
    let has_tool_error = state
        .messages()
        .iter()
        .any(|m| matches!(m, BaseMessage::Tool { is_error: true, .. }));
    assert!(has_tool_error, "取消后工具 error 消息应已写入 state");
}

/// 验证 HITL 拒绝（ToolRejected）不终止 Agent，LLM 能收到拒绝原因后继续
#[tokio::test]
async fn test_tool_rejection_continues_loop() {
    use crate::middleware::r#trait::Middleware;

    struct RejectAllMiddleware;
    #[async_trait::async_trait]
    impl<S: State> Middleware<S> for RejectAllMiddleware {
        fn name(&self) -> &str {
            "RejectAllMiddleware"
        }
        async fn before_tool(&self, _state: &mut S, tool_call: &ToolCall) -> AgentResult<ToolCall> {
            Err(AgentError::ToolRejected {
                tool: tool_call.name.clone(),
                reason: "用户拒绝".to_string(),
            })
        }
    }

    // LLM：先调用工具，收到拒绝结果后返回最终答案
    struct TestLLM;
    #[async_trait::async_trait]
    impl ReactLLM for TestLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> AgentResult<Reasoning> {
            let has_tool_result = messages
                .iter()
                .any(|m| matches!(m, BaseMessage::Tool { .. }));
            if !has_tool_result {
                Ok(Reasoning::with_tools(
                    "try tool",
                    vec![ToolCall::new(
                        "id1",
                        "Bash",
                        serde_json::json!({"command": "ls"}),
                    )],
                ))
            } else {
                Ok(Reasoning::with_answer("adjusted", "done after rejection"))
            }
        }
    }

    let agent = ReActAgent::new(TestLLM)
        .max_iterations(5)
        .add_middleware(Box::new(RejectAllMiddleware));

    let mut state = AgentState::new("/tmp");
    let output = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.text, "done after rejection");
    // 拒绝结果应写入 state（is_error=true）
    let has_rejection = state
        .messages()
        .iter()
        .any(|m| matches!(m, BaseMessage::Tool { is_error: true, .. }));
    assert!(has_rejection, "拒绝结果应写入 state");
    // Agent 总工具调用记录中应有 1 条（被拒绝的）
    assert_eq!(output.tool_calls.len(), 1);
}

/// 验证 TextChunk 携带的 message_id 与前一条 MessageAdded(Ai) 的 id 一致
#[tokio::test]
async fn test_text_chunk_message_id() {
    use std::sync::{Arc, Mutex};

    use crate::agent::events::{AgentEvent, FnEventHandler};

    struct FinalAnswerLLM;
    #[async_trait::async_trait]
    impl ReactLLM for FinalAnswerLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("thinking", "final answer"))
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let agent = ReActAgent::new(FinalAnswerLLM)
        .max_iterations(3)
        .with_event_handler(Arc::new(FnEventHandler(move |event| {
            events_clone.lock().unwrap().push(event);
        })));

    let mut state = AgentState::new("/tmp");
    agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();

    let evs = events.lock().unwrap();

    // 找到 MessageAdded(Ai) 的 id（最终答案那条）
    let ai_msg_id = evs.iter().find_map(|e| {
        if let AgentEvent::MessageAdded(BaseMessage::Ai { id, tool_calls, .. }) = e {
            if tool_calls.is_empty() {
                Some(*id)
            } else {
                None
            }
        } else {
            None
        }
    });

    // 找到 TextChunk 的 message_id
    let chunk_msg_id = evs.iter().find_map(|e| {
        if let AgentEvent::TextChunk { message_id, .. } = e {
            Some(*message_id)
        } else {
            None
        }
    });

    assert!(ai_msg_id.is_some(), "应有 MessageAdded(Ai) 事件");
    assert!(chunk_msg_id.is_some(), "应有 TextChunk 事件");
    assert_eq!(
        ai_msg_id.unwrap(),
        chunk_msg_id.unwrap(),
        "TextChunk.message_id 应与 MessageAdded(Ai).id 相同"
    );
}

/// 验证 ToolStart/ToolEnd 携带的 message_id 与同轮次 MessageAdded(Ai) 的 id 一致
#[tokio::test]
async fn test_tool_message_id() {
    use std::sync::{Arc, Mutex};

    use crate::agent::events::{AgentEvent, FnEventHandler};

    struct OneToolLLM;
    #[async_trait::async_trait]
    impl ReactLLM for OneToolLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            if messages
                .iter()
                .any(|m| matches!(m, BaseMessage::Tool { .. }))
            {
                Ok(Reasoning::with_answer("done", "ok"))
            } else {
                Ok(Reasoning::with_tools(
                    "call tool",
                    vec![ToolCall::new("tc1", "echo_tool", serde_json::json!({}))],
                ))
            }
        }
    }

    struct EchoTool;
    #[async_trait::async_trait]
    impl BaseTool for EchoTool {
        fn name(&self) -> &str {
            "echo_tool"
        }
        fn description(&self) -> &str {
            "echoes"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("echo".to_string())
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let agent = ReActAgent::new(OneToolLLM)
        .max_iterations(5)
        .register_tool(Box::new(EchoTool))
        .with_event_handler(Arc::new(FnEventHandler(move |event| {
            events_clone.lock().unwrap().push(event);
        })));

    let mut state = AgentState::new("/tmp");
    agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();

    let evs = events.lock().unwrap();

    // 找到第一个带工具调用的 MessageAdded(Ai) 的 id
    let ai_msg_id = evs.iter().find_map(|e| {
        if let AgentEvent::MessageAdded(BaseMessage::Ai { id, tool_calls, .. }) = e {
            if !tool_calls.is_empty() {
                Some(*id)
            } else {
                None
            }
        } else {
            None
        }
    });
    let tool_start_msg_id = evs.iter().find_map(|e| {
        if let AgentEvent::ToolStart { message_id, .. } = e {
            Some(*message_id)
        } else {
            None
        }
    });
    let tool_end_msg_id = evs.iter().find_map(|e| {
        if let AgentEvent::ToolEnd { message_id, .. } = e {
            Some(*message_id)
        } else {
            None
        }
    });

    assert!(
        ai_msg_id.is_some(),
        "应有带工具调用的 MessageAdded(Ai) 事件"
    );
    assert!(tool_start_msg_id.is_some(), "应有 ToolStart 事件");
    assert!(tool_end_msg_id.is_some(), "应有 ToolEnd 事件");
    assert_eq!(
        ai_msg_id.unwrap(),
        tool_start_msg_id.unwrap(),
        "ToolStart.message_id 应与 MessageAdded(Ai).id 相同"
    );
    assert_eq!(
        ai_msg_id.unwrap(),
        tool_end_msg_id.unwrap(),
        "ToolEnd.message_id 应与 MessageAdded(Ai).id 相同"
    );
}

/// 验证 with_system_prompt 注入的 system 消息在 execute 返回后被清理，
/// 不会累积到 history（回归测试：system 消息累积 bug）
#[tokio::test]
async fn test_system_prompt_is_first() {
    struct EchoLLM;
    #[async_trait::async_trait]
    impl ReactLLM for EchoLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("", "done"))
        }
    }

    let agent = ReActAgent::new(EchoLLM)
        .max_iterations(3)
        .with_system_prompt("system content here");

    let mut state = AgentState::new("/tmp");
    agent
        .execute(AgentInput::text("hi"), &mut state, None)
        .await
        .unwrap();

    // execute 返回后，prepend 的 system 消息应被清理
    let messages = state.messages();
    let system_count = messages.iter().filter(|m| m.is_system()).count();
    assert_eq!(
        system_count, 0,
        "execute 返回后不应残留 prepend 的 system 消息，实际有 {system_count} 条 system"
    );
}

/// 验证不论其他中间件注册顺序如何，with_system_prompt 的 system 消息始终在最前
#[tokio::test]
async fn test_system_prompt_order_independent() {
    use crate::middleware::r#trait::Middleware;

    // 一个会在 before_agent 中 prepend 自己消息的中间件
    struct PrefixMiddleware;
    #[async_trait::async_trait]
    impl<S: State> Middleware<S> for PrefixMiddleware {
        fn name(&self) -> &str {
            "PrefixMiddleware"
        }
        async fn before_agent(&self, state: &mut S) -> AgentResult<()> {
            state.prepend_message(BaseMessage::system("middleware injected"));
            Ok(())
        }
    }

    struct EchoLLM;
    #[async_trait::async_trait]
    impl ReactLLM for EchoLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("", "done"))
        }
    }

    // 中间件在 with_system_prompt 之前注册——但 system prompt 应在最前
    let agent = ReActAgent::new(EchoLLM)
        .add_middleware(Box::new(PrefixMiddleware))
        .with_system_prompt("top level system");

    let mut state = AgentState::new("/tmp");
    agent
        .execute(AgentInput::text("hi"), &mut state, None)
        .await
        .unwrap();

    let messages = state.messages();
    // execute() 结束后，prepend 的 system 消息应被清理，不应残留在 state 中
    let system_count = messages.iter().filter(|m| m.is_system()).count();
    assert_eq!(
        system_count, 0,
        "execute() 结束后 prepend 的 system 消息应被清理，实际残留 {} 条",
        system_count
    );
}

/// 回归测试：多次调用 execute() 时，prepend 的 system 消息不会跨调用累积
#[tokio::test]
async fn test_execute_no_system_accumulation_across_calls() {
    use crate::middleware::r#trait::Middleware;

    struct NoteMiddleware;
    #[async_trait::async_trait]
    impl<S: State> Middleware<S> for NoteMiddleware {
        fn name(&self) -> &str {
            "Note"
        }
        async fn before_agent(&self, state: &mut S) -> AgentResult<()> {
            state.prepend_message(BaseMessage::system("middleware note"));
            Ok(())
        }
    }

    struct EchoLLM;
    #[async_trait::async_trait]
    impl ReactLLM for EchoLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("", "ok"))
        }
    }

    let agent = ReActAgent::new(EchoLLM)
        .with_system_prompt("sys prompt".to_string())
        .add_middleware(Box::new(NoteMiddleware))
        .max_iterations(1);

    let mut state = AgentState::new("/tmp");

    // 第一次 execute
    agent
        .execute(AgentInput::text("first"), &mut state, None)
        .await
        .unwrap();
    let msgs_after_first = state.messages().len();

    // 第二次 execute
    agent
        .execute(AgentInput::text("second"), &mut state, None)
        .await
        .unwrap();
    let msgs_after_second = state.messages().len();

    // 两次 execute 各新增 +2（user + assistant），无 system 消息累积
    assert_eq!(
        msgs_after_second - msgs_after_first,
        2,
        "第二次 execute 应只增加 2 条消息（user + assistant），实际增加 {} 条",
        msgs_after_second - msgs_after_first
    );

    let system_count = state.messages().iter().filter(|m| m.is_system()).count();
    assert_eq!(
        system_count, 0,
        "多次 execute() 后不应有 system 消息累积，实际有 {} 条",
        system_count
    );
}

/// 验证 TextChunk/ToolStart/ToolEnd 序列化后含 message_id 字段
#[test]
fn test_agent_event_message_id_serialization() {
    use crate::{agent::events::AgentEvent, messages::MessageId};

    let mid = MessageId::new();

    let ev = AgentEvent::TextChunk {
        message_id: mid,
        chunk: "hello".to_string(),
        source_agent_id: None,
    };
    let json = serde_json::to_value(&ev).unwrap();
    // Adjacently tagged: fields are inside json["value"]
    let content = &json["value"];
    assert!(
        content["message_id"].is_string(),
        "TextChunk JSON 应含 message_id 字段"
    );
    assert_eq!(content["chunk"].as_str().unwrap(), "hello");

    let ev = AgentEvent::ToolStart {
        message_id: mid,
        tool_call_id: "tc1".to_string(),
        name: "Bash".to_string(),
        input: serde_json::json!({}),
        source_agent_id: None,
    };
    let json = serde_json::to_value(&ev).unwrap();
    let content = &json["value"];
    assert!(
        content["message_id"].is_string(),
        "ToolStart JSON 应含 message_id 字段"
    );

    let ev = AgentEvent::ToolEnd {
        message_id: mid,
        tool_call_id: "tc1".to_string(),
        name: "Bash".to_string(),
        output: "ok".to_string(),
        is_error: false,
        source_agent_id: None,
    };
    let json = serde_json::to_value(&ev).unwrap();
    let content = &json["value"];
    assert!(
        content["message_id"].is_string(),
        "ToolEnd JSON 应含 message_id 字段"
    );
}

/// 验证最终回答路径也会发出 StateSnapshot，确保多轮对话不丢失 AI 回复
#[tokio::test]
async fn test_state_snapshot_on_final_answer() {
    use std::sync::{Arc, Mutex};

    use crate::agent::events::{AgentEvent, FnEventHandler};

    struct FinalAnswerLLM;
    #[async_trait::async_trait]
    impl ReactLLM for FinalAnswerLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("thinking", "final answer"))
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let agent = ReActAgent::new(FinalAnswerLLM)
        .max_iterations(3)
        .with_event_handler(Arc::new(FnEventHandler(move |event| {
            events_clone.lock().unwrap().push(event);
        })));

    let mut state = AgentState::new("/tmp");
    agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();

    let evs = events.lock().unwrap();
    let snapshots: Vec<_> = evs
        .iter()
        .filter(|e| matches!(e, AgentEvent::StateSnapshot(_)))
        .collect();

    assert!(!snapshots.is_empty(), "最终回答路径应发出 StateSnapshot");

    // 验证只有一个 snapshot 包含 AI 最终回答（无重复）
    let ai_final_count = evs
        .iter()
        .filter(|e| {
            if let AgentEvent::StateSnapshot(msgs) = e {
                msgs.iter().any(
                    |m| matches!(m, BaseMessage::Ai { tool_calls, .. } if tool_calls.is_empty()),
                )
            } else {
                false
            }
        })
        .count();
    assert_eq!(
        ai_final_count, 1,
        "AI 最终回答应只出现在一个 StateSnapshot 中（实际: {ai_final_count}）"
    );

    // 最后一个 snapshot 应包含 AI 最终回答
    if let AgentEvent::StateSnapshot(msgs) = snapshots.last().unwrap() {
        let has_ai_text = msgs
            .iter()
            .any(|m| matches!(m, BaseMessage::Ai { tool_calls, .. } if tool_calls.is_empty()));
        assert!(has_ai_text, "StateSnapshot 应包含不带工具调用的 AI 消息");
    }
}

/// 验证达到最大迭代次数时返回 MaxIterationsExceeded 错误
#[tokio::test]
async fn test_max_iterations_exceeded() {
    struct AlwaysToolLLM;
    #[async_trait::async_trait]
    impl ReactLLM for AlwaysToolLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_tools(
                "loop",
                vec![ToolCall::new("id1", "echo_tool", serde_json::json!({}))],
            ))
        }
    }

    struct EchoTool;
    #[async_trait::async_trait]
    impl BaseTool for EchoTool {
        fn name(&self) -> &str {
            "echo_tool"
        }
        fn description(&self) -> &str {
            "echoes"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("echo".to_string())
        }
    }

    let agent = ReActAgent::new(AlwaysToolLLM)
        .max_iterations(3)
        .register_tool(Box::new(EchoTool));

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await;

    assert!(matches!(result, Err(AgentError::MaxIterationsExceeded(3))));
    // 1 human + 3*(ai + tool_result)
    assert_eq!(state.messages().len(), 7);
}

/// 验证两个工具调用通过批量 before_tools_batch 处理（HITL 批量审批路径）
#[tokio::test]
async fn test_batch_before_tools_execution() {
    use std::sync::{Arc, Mutex};

    use crate::agent::events::{AgentEvent, FnEventHandler};

    struct TwoToolLLM;
    #[async_trait::async_trait]
    impl ReactLLM for TwoToolLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            if messages
                .iter()
                .any(|m| matches!(m, BaseMessage::Tool { .. }))
            {
                Ok(Reasoning::with_answer("done", "ok"))
            } else {
                Ok(Reasoning::with_tools(
                    "need both",
                    vec![
                        ToolCall::new("id1", "tool_a", serde_json::json!({})),
                        ToolCall::new("id2", "tool_b", serde_json::json!({})),
                    ],
                ))
            }
        }
    }

    struct EchoTool {
        name_str: &'static str,
    }
    #[async_trait::async_trait]
    impl BaseTool for EchoTool {
        fn name(&self) -> &str {
            self.name_str
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(format!("{} done", self.name_str))
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let agent = ReActAgent::new(TwoToolLLM)
        .max_iterations(5)
        .register_tool(Box::new(EchoTool { name_str: "tool_a" }))
        .register_tool(Box::new(EchoTool { name_str: "tool_b" }))
        .with_event_handler(Arc::new(FnEventHandler(move |event| {
            events_clone.lock().unwrap().push(event);
        })));

    let mut state = AgentState::new("/tmp");
    let output = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();

    assert_eq!(output.text, "ok");
    assert_eq!(output.tool_calls.len(), 2);

    let evs = events.lock().unwrap();
    let tool_starts: Vec<_> = evs
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolStart { .. }))
        .collect();
    assert_eq!(tool_starts.len(), 2, "应有 2 个 ToolStart 事件");
}

/// 验证 with_context_budget 设置后 executor 使用 ContextBudget 阈值
#[tokio::test]
async fn test_context_budget_wiring() {
    struct TokenLLM {
        input_tokens: u32,
        output_tokens: u32,
    }
    #[async_trait::async_trait]
    impl ReactLLM for TokenLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            let mut r = Reasoning::with_answer("", "ok");
            r.usage = Some(crate::llm::types::TokenUsage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
                request_id: None,
            });
            Ok(r)
        }
    }

    // context_window=1000, warning_threshold=0.5 → 600/1000=60% > 50%
    let budget = crate::agent::token::ContextBudget::new(1000).with_warning_threshold(0.5);
    let agent = ReActAgent::new(TokenLLM {
        input_tokens: 400,
        output_tokens: 200,
    })
    .max_iterations(3)
    .with_context_budget(budget);
    let mut state = AgentState::new("/tmp");
    let output = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(output.text, "ok");
    let t = state.token_tracker();
    assert_eq!(t.total_input_tokens, 400);
    assert_eq!(t.total_output_tokens, 200);
    assert_eq!(t.llm_call_count, 1);
}

/// 验证无 ContextBudget 时回退到硬编码 80% 阈值（向后兼容）
#[tokio::test]
async fn test_no_context_budget_fallback() {
    struct LowTokenLLM;
    #[async_trait::async_trait]
    impl ReactLLM for LowTokenLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            let mut r = Reasoning::with_answer("", "ok");
            r.usage = Some(crate::llm::types::TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
                request_id: None,
            });
            Ok(r)
        }
    }
    let agent = ReActAgent::new(LowTokenLLM).max_iterations(3);
    let mut state = AgentState::new("/tmp");
    let output = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(output.text, "ok");
    assert_eq!(state.token_tracker().llm_call_count, 1);
}

/// 验证 ContextBudget 路径下 ContextWarning 事件被发出
#[tokio::test]
async fn test_context_budget_emits_warning_event() {
    use std::sync::{Arc, Mutex};

    use crate::agent::events::{AgentEvent, FnEventHandler};

    struct TokenLLM {
        input_tokens: u32,
        output_tokens: u32,
    }
    #[async_trait::async_trait]
    impl ReactLLM for TokenLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            let mut r = Reasoning::with_answer("", "ok");
            r.usage = Some(crate::llm::types::TokenUsage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
                request_id: None,
            });
            Ok(r)
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    // context_window=1000, warning_threshold=0.5 → input=600/1000=60% > 50%
    let budget = crate::agent::token::ContextBudget::new(1000).with_warning_threshold(0.5);
    let agent = ReActAgent::new(TokenLLM {
        input_tokens: 600,
        output_tokens: 200,
    })
    .max_iterations(3)
    .with_context_budget(budget)
    .with_event_handler(Arc::new(FnEventHandler(move |ev| {
        events_clone.lock().unwrap().push(ev);
    })));

    let mut state = AgentState::new("/tmp");
    let output = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(output.text, "ok");

    let evs = events.lock().unwrap();
    let warnings: Vec<_> = evs
        .iter()
        .filter(|e| matches!(e, AgentEvent::ContextWarning { .. }))
        .collect();
    assert_eq!(warnings.len(), 1, "ContextWarning 应在超过警告阈值时发出");
    if let AgentEvent::ContextWarning {
        used_tokens,
        total_tokens,
        percentage,
    } = warnings[0]
    {
        assert_eq!(*used_tokens, 600, "used_tokens = input = 600");
        assert_eq!(*total_tokens, 1000, "total_tokens = budget.context_window");
        assert!((*percentage - 60.0).abs() < 1.0, "percentage ≈ 60%");
    }
}

/// 验证无 ContextBudget 时回退路径也发出 ContextWarning 事件
#[tokio::test]
async fn test_fallback_path_emits_warning_event() {
    use std::sync::{Arc, Mutex};

    use crate::agent::events::{AgentEvent, FnEventHandler};

    struct HighTokenLLM;
    #[async_trait::async_trait]
    impl ReactLLM for HighTokenLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            let mut r = Reasoning::with_answer("", "ok");
            // context_window 默认 200K，input=170K → 170K/200K = 85% > 80% 硬编码阈值
            r.usage = Some(crate::llm::types::TokenUsage {
                input_tokens: 170000,
                output_tokens: 80000,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
                request_id: None,
            });
            Ok(r)
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    let agent = ReActAgent::new(HighTokenLLM)
        .max_iterations(3)
        .with_event_handler(Arc::new(FnEventHandler(move |ev| {
            events_clone.lock().unwrap().push(ev);
        })));

    let mut state = AgentState::new("/tmp");
    let output = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(output.text, "ok");

    let evs = events.lock().unwrap();
    let warnings: Vec<_> = evs
        .iter()
        .filter(|e| matches!(e, AgentEvent::ContextWarning { .. }))
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "无 budget 时回退路径也应发出 ContextWarning"
    );
    if let AgentEvent::ContextWarning {
        used_tokens,
        total_tokens: _,
        percentage,
    } = warnings[0]
    {
        assert_eq!(*used_tokens, 170000, "used_tokens = input = 170K");
        assert!((*percentage - 85.0).abs() < 1.0, "percentage ≈ 85%");
    }
}

/// 验证低 token 用量时不发出 ContextWarning
#[tokio::test]
async fn test_low_usage_no_warning_event() {
    use std::sync::{Arc, Mutex};

    use crate::agent::events::{AgentEvent, FnEventHandler};

    struct LowTokenLLM;
    #[async_trait::async_trait]
    impl ReactLLM for LowTokenLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            let mut r = Reasoning::with_answer("", "ok");
            r.usage = Some(crate::llm::types::TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
                request_id: None,
            });
            Ok(r)
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    let agent = ReActAgent::new(LowTokenLLM)
        .max_iterations(3)
        .with_event_handler(Arc::new(FnEventHandler(move |ev| {
            events_clone.lock().unwrap().push(ev);
        })));

    let mut state = AgentState::new("/tmp");
    let output = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(output.text, "ok");

    let evs = events.lock().unwrap();
    // LLM 必然有 LlmCallEnd，但 low usage 不触发 ContextWarning
    let has_warning = evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ContextWarning { .. }));
    assert!(!has_warning, "低 token 用量不应发出 ContextWarning");
}

/// 验证 executor 发射的 StateSnapshot 之间无消息重叠。
/// 这是修复 agent_state_messages 消息重复的核心保障：
/// 增量快照之间不应包含相同 message_id 的消息。
#[tokio::test]
async fn test_state_snapshot_no_overlap() {
    use std::sync::{Arc, Mutex};

    use crate::agent::events::{AgentEvent, FnEventHandler};

    struct ToolThenAnswerLLM;
    #[async_trait::async_trait]
    impl ReactLLM for ToolThenAnswerLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            let has_tool_result = messages
                .iter()
                .any(|m| matches!(m, BaseMessage::Tool { .. }));
            if !has_tool_result {
                Ok(Reasoning::with_tools(
                    "need tool",
                    vec![ToolCall::new("id1", "echo_tool", serde_json::json!({}))],
                ))
            } else {
                Ok(Reasoning::with_answer("done", "tool result received"))
            }
        }
    }

    struct EchoTool;
    #[async_trait::async_trait]
    impl BaseTool for EchoTool {
        fn name(&self) -> &str {
            "echo_tool"
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("echo result".to_string())
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let agent = ReActAgent::new(ToolThenAnswerLLM)
        .max_iterations(10)
        .register_tool(Box::new(EchoTool))
        .with_event_handler(Arc::new(FnEventHandler(move |event| {
            events_clone.lock().unwrap().push(event);
        })));

    let mut state = AgentState::new("/tmp");
    agent
        .execute(AgentInput::text("test"), &mut state, None)
        .await
        .unwrap();

    let evs = events.lock().unwrap();
    let snapshots: Vec<Vec<BaseMessage>> = evs
        .iter()
        .filter_map(|e| {
            if let AgentEvent::StateSnapshot(msgs) = e {
                Some(msgs.clone())
            } else {
                None
            }
        })
        .collect();

    assert!(!snapshots.is_empty(), "应至少有一个 StateSnapshot");

    // 将所有 snapshot 的消息展平，验证无重复的 message_id
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut dup_count = 0;
    for snapshot in &snapshots {
        for msg in snapshot {
            let id = msg.id().as_uuid().to_string();
            if !seen_ids.insert(id) {
                dup_count += 1;
            }
        }
    }
    assert_eq!(
        dup_count, 0,
        "StateSnapshot 之间不应有重叠消息（重复 message_id 数量: {dup_count}）"
    );
}

/// 验证 StateSnapshot 不包含 System 消息
///
/// 回归测试：prepend_message 的 insert(0) 会右移所有元素，
/// 导致 messages[last_message_count..] 在空 history 场景下包含
/// 被 prepend 的 System 消息。这些消息不应泄露到 agent_state_messages。
#[tokio::test]
async fn test_state_snapshot_excludes_system_messages() {
    use std::sync::{Arc, Mutex};

    use crate::{
        agent::events::{AgentEvent, FnEventHandler},
        middleware::r#trait::Middleware,
    };

    struct AnswerLLM;
    #[async_trait::async_trait]
    impl ReactLLM for AnswerLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("done", "final answer"))
        }
    }

    // 收集所有事件的中间件，用于验证 before_agent 是否执行
    struct PrependSystemMiddleware;
    #[async_trait::async_trait]
    impl<S: crate::agent::state::State> Middleware<S> for PrependSystemMiddleware {
        fn name(&self) -> &str {
            "PrependSystemMiddleware"
        }
        async fn before_agent(&self, state: &mut S) -> crate::error::AgentResult<()> {
            state.prepend_message(BaseMessage::system("middleware injected system content"));
            Ok(())
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let agent = ReActAgent::new(AnswerLLM)
        .max_iterations(10)
        .with_system_prompt("main system prompt".to_string())
        .add_middleware(Box::new(PrependSystemMiddleware))
        .with_event_handler(Arc::new(FnEventHandler(move |event| {
            events_clone.lock().unwrap().push(event);
        })));

    // 空 history 场景（新会话首条消息）
    let mut state = AgentState::new("/tmp");
    agent
        .execute(AgentInput::text("test"), &mut state, None)
        .await
        .unwrap();

    let evs = events.lock().unwrap();
    let snapshots: Vec<Vec<BaseMessage>> = evs
        .iter()
        .filter_map(|e| {
            if let AgentEvent::StateSnapshot(msgs) = e {
                Some(msgs.clone())
            } else {
                None
            }
        })
        .collect();

    assert!(!snapshots.is_empty(), "应至少有一个 StateSnapshot");

    // 所有 StateSnapshot 中不应包含任何 System 消息
    for (i, snapshot) in snapshots.iter().enumerate() {
        for (j, msg) in snapshot.iter().enumerate() {
            assert!(
                !msg.is_system(),
                "StateSnapshot[{i}][{j}] 不应包含 System 消息，但发现: {:?}",
                msg.content().chars().take(50).collect::<String>()
            );
        }
    }
}

// ─── set_* per-turn update 方法测试 ──────────────────────────────────────────

/// 验证 set_event_handler 在 &mut agent 上替换事件回调
#[tokio::test]
async fn test_set_event_handler() {
    use std::sync::{Arc, Mutex};

    use crate::agent::events::FnEventHandler;

    struct AnswerLLM;
    #[async_trait::async_trait]
    impl ReactLLM for AnswerLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("", "ok"))
        }
    }

    let events_a: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let events_b: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let events_a_clone = events_a.clone();
    let events_b_clone = events_b.clone();

    let mut agent = ReActAgent::new(AnswerLLM).max_iterations(3);

    // 第一次：无 handler，静默
    let mut state = AgentState::new("/tmp");
    agent
        .execute(AgentInput::text("first"), &mut state, None)
        .await
        .unwrap();
    assert!(
        events_a.lock().unwrap().is_empty(),
        "无 handler 时不应收集事件"
    );

    // set_event_handler: 切换到 handler_a
    agent.set_event_handler(Arc::new(FnEventHandler(move |ev| {
        if let AgentEvent::TextChunk { chunk, .. } = ev {
            events_a_clone.lock().unwrap().push(chunk);
        }
    })));
    agent
        .execute(AgentInput::text("second"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(
        events_a.lock().unwrap().len(),
        1,
        "handler_a 应收到第二次 execute 的 TextChunk"
    );

    // set_event_handler: 切换到 handler_b
    agent.set_event_handler(Arc::new(FnEventHandler(move |ev| {
        if let AgentEvent::TextChunk { chunk, .. } = ev {
            events_b_clone.lock().unwrap().push(chunk);
        }
    })));
    agent
        .execute(AgentInput::text("third"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(
        events_b.lock().unwrap().len(),
        1,
        "handler_b 应收到第三次 execute 的 TextChunk"
    );
    // handler_a 不应收到后续事件（长度仍为 1）
    assert_eq!(
        events_a.lock().unwrap().len(),
        1,
        "handler_a 不应收到切换后的事件"
    );
}

/// 验证 set_system_prompt 在 &mut agent 上设置系统提示词（仅可调用一次）
///
/// 系统提示词稳定性是第一优先级不变量：session/new 时一次性冻结，
/// 多次调用 set_system_prompt 会触发 debug_assert panic。
#[tokio::test]
async fn test_set_system_prompt() {
    struct AnswerLLM;
    #[async_trait::async_trait]
    impl ReactLLM for AnswerLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("", "ok"))
        }
    }

    let mut agent = ReActAgent::new(AnswerLLM).max_iterations(3);
    let mut state = AgentState::new("/tmp");

    // 第一次：无 system prompt
    agent
        .execute(AgentInput::text("first"), &mut state, None)
        .await
        .unwrap();

    // set_system_prompt（首次调用，合法）
    agent.set_system_prompt("frozen system prompt");
    agent
        .execute(AgentInput::text("second"), &mut state, None)
        .await
        .unwrap();

    // 验证 system 消息未累积（prepend 后清理）
    let system_count = state.messages().iter().filter(|m| m.is_system()).count();
    assert_eq!(
        system_count, 0,
        "set_system_prompt 后 system 消息不应累积，实际有 {system_count} 条"
    );
}

/// 验证 set_notification_rx 在 &mut agent 上更新通知接收端
#[tokio::test]
async fn test_set_notification_rx() {
    struct AnswerLLM;
    #[async_trait::async_trait]
    impl ReactLLM for AnswerLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("", "ok"))
        }
    }

    let mut agent = ReActAgent::new(AnswerLLM).max_iterations(3);
    assert!(
        agent.notification_rx.is_none(),
        "新建 agent 的 notification_rx 应为 None"
    );

    let (_tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
    agent.set_notification_rx(rx1);
    assert!(
        agent.notification_rx.is_some(),
        "set_notification_rx 后应为 Some"
    );

    // 可重复调用替换
    let (_tx2, rx2) = tokio::sync::mpsc::unbounded_channel();
    agent.set_notification_rx(rx2);
    assert!(
        agent.notification_rx.is_some(),
        "重复 set_notification_rx 后应仍为 Some"
    );

    // 功能验证：更新后的 agent 仍可正常执行
    let mut state = AgentState::new("/tmp");
    let output = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await
        .unwrap();
    assert_eq!(output.text, "ok");
}

// ─── A4: LLM 错误路径下 cleanup_prepended 行为测试 ──────────────────────

/// 验证 LLM 返回错误时 execute() 的 ? 传播行为：
/// LLM 错误（如 400）通过 ? 传播出函数，跳过 cleanup_prepended，
/// 导致 prepend 的 system 消息泄漏到 state。ACP 层通过
/// strip_leaked_prepends 补偿清理此泄漏。
///
/// 此测试验证当前行为，确保未来修改 executor 清理逻辑时与 ACP 层协调。
#[tokio::test]
async fn test_llm_error_cleanup_prepended_behavior() {
    use crate::middleware::r#trait::Middleware;

    // 中间件：before_agent 中 prepend system 消息
    struct PrependSystemMiddleware;
    #[async_trait::async_trait]
    impl<S: State> Middleware<S> for PrependSystemMiddleware {
        fn name(&self) -> &str {
            "PrependSystem"
        }
        async fn before_agent(&self, state: &mut S) -> AgentResult<()> {
            state.prepend_message(BaseMessage::system("middleware prepend content"));
            Ok(())
        }
    }

    // LLM：第一次调用就返回 400 错误
    struct ErrorLLM;
    #[async_trait::async_trait]
    impl ReactLLM for ErrorLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> AgentResult<Reasoning> {
            Err(AgentError::LlmHttpError {
                status: 400,
                message: "模拟 LLM 错误".to_string(),
            })
        }
    }

    let agent = ReActAgent::new(ErrorLLM)
        .max_iterations(5)
        .with_system_prompt("main system prompt".to_string())
        .add_middleware(Box::new(PrependSystemMiddleware));

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("hello"), &mut state, None)
        .await;

    // LLM 错误应正确传播
    assert!(
        matches!(&result, Err(AgentError::LlmHttpError { status: 400, .. })),
        "应返回 LlmHttpError(400)，实际: {:?}",
        result
    );

    // A4 修复后验证：cleanup_prepended 在错误路径上也执行，system 消息被正确清理
    let messages = state.messages();
    let system_count = messages.iter().filter(|m| m.is_system()).count();
    let human_count = messages
        .iter()
        .filter(|m| matches!(m, BaseMessage::Human { .. }))
        .count();

    assert_eq!(human_count, 1, "state 中应有 1 条 Human 消息");
    assert_eq!(
        system_count, 0,
        "LLM 错误路径下 system 消息应被清理，实际有 {system_count} 条"
    );
}

// ─── P0-2: shared_tools 每轮 clear 测试 ──────────────────────────────────

/// 验证 shared_tools 在每轮 execute 开始时被 clear，已断开工具不会永久残留。
#[tokio::test]
async fn test_shared_tools_clears_between_turns() {
    use crate::middleware::r#trait::Middleware;

    struct AnswerLLM;
    #[async_trait::async_trait]
    impl ReactLLM for AnswerLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> crate::error::AgentResult<Reasoning> {
            Ok(Reasoning::with_answer("", "ok"))
        }
    }

    struct GhostTool;
    #[async_trait::async_trait]
    impl BaseTool for GhostTool {
        fn name(&self) -> &str {
            "ghost_tool_from_prev_turn"
        }
        fn description(&self) -> &str {
            "ghost"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("ok".to_string())
        }
    }

    /// 提供当前轮工具的中间件（shared_tools 通过 middleware_tools 写入，不走 self.tools）
    struct CurrentTurnMiddleware;
    #[async_trait::async_trait]
    impl<S: crate::agent::state::State> Middleware<S> for CurrentTurnMiddleware {
        fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
            vec![Box::new(GhostTool) as Box<dyn BaseTool>]
        }
        fn name(&self) -> &str {
            "CurrentTurnMiddleware"
        }
    }

    let shared_tools: Arc<parking_lot::RwLock<HashMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(parking_lot::RwLock::new(HashMap::new()));

    // 第一轮：CurrentTurnMiddleware 提供 ghost_tool，写入 shared_tools
    let agent_v1 = ReActAgent::new(AnswerLLM)
        .max_iterations(1)
        .add_middleware(Box::new(CurrentTurnMiddleware))
        .with_shared_tools(Arc::clone(&shared_tools));

    let mut state = AgentState::new("/tmp");
    agent_v1
        .execute(AgentInput::text("turn1"), &mut state, None)
        .await
        .unwrap();
    {
        let map = shared_tools.read();
        assert!(
            map.contains_key("ghost_tool_from_prev_turn"),
            "第一轮后 ghost_tool 应在 shared_tools 中，keys: {:?}",
            map.keys().collect::<Vec<_>>()
        );
    }

    // 第二轮：去掉 CurrentTurnMiddleware（模拟 MCP 断开），ghost_tool 不再由 collect_tools 返回
    // 若没有 clear，ghost_tool 会永久残留
    let agent_v2 = ReActAgent::new(AnswerLLM)
        .max_iterations(1)
        .with_shared_tools(Arc::clone(&shared_tools));

    agent_v2
        .execute(AgentInput::text("turn2"), &mut state, None)
        .await
        .unwrap();
    {
        let map = shared_tools.read();
        assert!(
            !map.contains_key("ghost_tool_from_prev_turn"),
            "P0-2 修复：第二轮 clear 后 ghost_tool 应被清除，实际 keys: {:?}",
            map.keys().collect::<Vec<_>>()
        );
    }
}
