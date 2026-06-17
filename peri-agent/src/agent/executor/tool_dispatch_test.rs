use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use super::*;
use crate::{
    agent::{
        events::{AgentEvent, FnEventHandler},
        react::{AgentInput, Reasoning},
        state::AgentState,
    },
    middleware::r#trait::Middleware,
    tools::BaseTool,
};

// ─── Test Fixture Factory ───────────────────────────────────────────────
// 共享 Mock 工具与 LLM，消除原本散布在各测试函数内的重复定义。

/// EchoTool：按传入名称返回 "<name> done"。
pub struct MockEchoTool {
    name_str: &'static str,
}
#[async_trait::async_trait]
impl BaseTool for MockEchoTool {
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
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(format!("{} done", self.name_str))
    }
}

/// 工厂函数：创建名为 `name` 的 EchoTool。
fn make_echo_tool(name: &'static str) -> MockEchoTool {
    MockEchoTool { name_str: name }
}

/// ThreeToolLLM：首轮发起 id1/id2/id3 三个工具调用，
/// 看到任意 tool_result 后回传最终答案 `final_answer`。
pub struct MockThreeToolLLM {
    final_answer: &'static str,
}
#[async_trait::async_trait]
impl ReactLLM for MockThreeToolLLM {
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
                "call three tools",
                vec![
                    ToolCall::new("id1", "tool_a", serde_json::json!({})),
                    ToolCall::new("id2", "tool_b", serde_json::json!({})),
                    ToolCall::new("id3", "tool_c", serde_json::json!({})),
                ],
            ))
        } else {
            Ok(Reasoning::with_answer("done", self.final_answer))
        }
    }
}

/// 工厂函数：创建最终答案为 `final_answer` 的 ThreeToolLLM。
fn make_three_tool_llm(final_answer: &'static str) -> MockThreeToolLLM {
    MockThreeToolLLM { final_answer }
}

/// TwoToolLLM：首轮发起 id1/id2 两个工具调用，
/// 看到任意 tool_result 后回传最终答案 `final_answer`。
pub struct MockTwoToolLLM {
    final_answer: &'static str,
}
#[async_trait::async_trait]
impl ReactLLM for MockTwoToolLLM {
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
                "call two tools",
                vec![
                    ToolCall::new("id1", "tool_a", serde_json::json!({})),
                    ToolCall::new("id2", "tool_b", serde_json::json!({})),
                ],
            ))
        } else {
            Ok(Reasoning::with_answer("done", self.final_answer))
        }
    }
}

/// 工厂函数：创建最终答案为 `final_answer` 的 TwoToolLLM。
fn make_two_tool_llm(final_answer: &'static str) -> MockTwoToolLLM {
    MockTwoToolLLM { final_answer }
}

/// 通用不变量：state 中每个 tool_use 必须有对应 tool_result。
fn assert_no_orphaned_tool_uses(state: &AgentState) {
    let mut ai_tool_ids: Vec<String> = Vec::new();
    let mut tool_result_ids: Vec<String> = Vec::new();
    for msg in state.messages() {
        if let BaseMessage::Ai { tool_calls, .. } = msg {
            for tc in tool_calls {
                ai_tool_ids.push(tc.id.clone());
            }
        }
        if let BaseMessage::Tool { tool_call_id, .. } = msg {
            tool_result_ids.push(tool_call_id.clone());
        }
    }
    assert_eq!(
        ai_tool_ids.len(),
        tool_result_ids.len(),
        "tool_use 数量 ({}) != tool_result 数量 ({})\n\
         tool_use IDs: {:?}\n\
         tool_result IDs: {:?}",
        ai_tool_ids.len(),
        tool_result_ids.len(),
        ai_tool_ids,
        tool_result_ids
    );
    for id in &ai_tool_ids {
        assert!(
            tool_result_ids.contains(id),
            "tool_use id={} 缺少配对 tool_result（孤儿 tool_use → Anthropic API 400）",
            id
        );
    }
}

/// 并发工具执行中部分失败：3 个工具并发，tool_b 执行失败。
/// 验证所有 tool_use 都有配对 tool_result，Agent 继续（不停止）。
#[tokio::test]
async fn test_concurrent_partial_failure_all_results_written() {
    struct FailToolB;
    #[async_trait::async_trait]
    impl BaseTool for FailToolB {
        fn name(&self) -> &str {
            "tool_b"
        }
        fn description(&self) -> &str {
            "fails"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Err("tool_b 执行失败".into())
        }
    }

    let agent = ReActAgent::new(make_three_tool_llm("all results processed"))
        .max_iterations(5)
        .register_tool(Box::new(make_echo_tool("tool_a")))
        .register_tool(Box::new(FailToolB))
        .register_tool(Box::new(make_echo_tool("tool_c")));

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await;

    assert!(result.is_ok(), "Agent 应正常完成，实际: {:?}", result);
    assert_no_orphaned_tool_uses(&state);
}

/// 验证 before_tool 非拒绝错误（P4 路径）在 i>0 时，
/// 已通过 before_tool 的 modified_calls 也获得 error tool_result，
/// 不产生孤儿 tool_use（Anthropic API 400）。
///
/// 场景：3 个工具调用，call[0] 通过 before_tool（推入 modified_calls），
/// call[1] 的 before_tool 返回非 ToolRejected 错误 → P3 路径触发。
/// 修复前：call[0] 成为孤儿 tool_use；修复后：call[0] 也获得 error tool_result。
#[tokio::test]
async fn test_p3_error_flushes_modified_calls_no_orphaned_tool_use() {
    // 中间件：第一个工具通过，后续全部返回非 ToolRejected 错误
    struct PartialFailMiddleware;
    #[async_trait::async_trait]
    impl<S: State> Middleware<S> for PartialFailMiddleware {
        fn name(&self) -> &str {
            "PartialFailMiddleware"
        }
        async fn before_tool(&self, _state: &mut S, tool_call: &ToolCall) -> AgentResult<ToolCall> {
            if tool_call.id == "id1" {
                // 第一个工具通过
                Ok(tool_call.clone())
            } else {
                // 后续工具返回非 ToolRejected 错误
                Err(AgentError::ToolExecutionFailed {
                    tool: tool_call.name.clone(),
                    reason: "模拟 before_tool 错误".to_string(),
                })
            }
        }
    }

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let agent = ReActAgent::new(make_three_tool_llm("all results received"))
        .max_iterations(5)
        .register_tool(Box::new(make_echo_tool("tool_a")))
        .register_tool(Box::new(make_echo_tool("tool_b")))
        .register_tool(Box::new(make_echo_tool("tool_c")))
        .add_middleware(Box::new(PartialFailMiddleware))
        .with_event_handler(Arc::new(FnEventHandler(move |event| {
            events_clone.lock().unwrap().push(event);
        })));

    let mut state = AgentState::new("/tmp");
    // P3 路径返回错误，execute 应传播该错误
    let result = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await;

    // P3 路径返回错误，execute 应传播该错误
    assert!(result.is_err(), "P3 路径应返回错误，实际: {:?}", result);

    // 延迟写入：before_tool 错误路径不写入 AI 消息到 state
    assert_no_orphaned_tool_uses(&state);
    // AI 消息未写入，state 中无 tool_use 也无 tool_result
    let ai_count = state
        .messages()
        .iter()
        .filter(|m| matches!(m, BaseMessage::Ai { .. }))
        .count();
    assert_eq!(ai_count, 0, "before_tool 错误路径不应写入 AI 消息到 state");
}

/// 验证取消信号在 i>0 时，modified_calls 也获得 error tool_result。
#[tokio::test]
async fn test_cancel_at_i_gt_0_flushes_modified_calls() {
    struct SlowTool;
    #[async_trait::async_trait]
    impl BaseTool for SlowTool {
        fn name(&self) -> &str {
            "slow_tool"
        }
        fn description(&self) -> &str {
            "hangs in before_tool then in execution"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok("never".to_string())
        }
    }

    // 中间件：第一个工具通过 before_tool 但后续挂起
    struct HangingBeforeToolMiddleware {
        call_count: Arc<Mutex<usize>>,
    }
    #[async_trait::async_trait]
    impl<S: State> Middleware<S> for HangingBeforeToolMiddleware {
        fn name(&self) -> &str {
            "HangingBeforeToolMiddleware"
        }
        async fn before_tool(
            &self,
            _state: &mut S,
            _tool_call: &ToolCall,
        ) -> AgentResult<ToolCall> {
            let should_hang = {
                let mut count = self.call_count.lock().unwrap();
                *count += 1;
                *count > 1
            };
            // guard 已在块内释放
            if should_hang {
                // 后续工具挂起（等待取消），200ms 足够让 cancel (100ms) 触发
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Ok(_tool_call.clone())
        }
    }

    struct TwoToolLLM;
    #[async_trait::async_trait]
    impl ReactLLM for TwoToolLLM {
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
                    "call two tools",
                    vec![
                        ToolCall::new("id1", "slow_tool", serde_json::json!({})),
                        ToolCall::new("id2", "slow_tool", serde_json::json!({})),
                    ],
                ))
            } else {
                Ok(Reasoning::with_answer("done", "ok"))
            }
        }
    }

    let cancel = CancellationToken::new();
    let call_count = Arc::new(Mutex::new(0usize));
    let agent = ReActAgent::new(TwoToolLLM)
        .max_iterations(5)
        .register_tool(Box::new(SlowTool))
        .add_middleware(Box::new(HangingBeforeToolMiddleware {
            call_count: Arc::clone(&call_count),
        }));

    // 在 before_tool 处理第二个工具时触发取消
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    });

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, Some(cancel))
        .await;

    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "取消后应返回 Interrupted，实际: {:?}",
        result
    );

    // 核心断言：所有 tool_use 必须有配对 tool_result
    let mut ai_tool_ids: Vec<String> = Vec::new();
    let mut tool_result_ids: Vec<String> = Vec::new();
    for msg in state.messages() {
        if let BaseMessage::Ai { tool_calls, .. } = msg {
            for tc in tool_calls {
                ai_tool_ids.push(tc.id.clone());
            }
        }
        if let BaseMessage::Tool { tool_call_id, .. } = msg {
            tool_result_ids.push(tool_call_id.clone());
        }
    }

    for id in &ai_tool_ids {
        assert!(
            tool_result_ids.contains(id),
            "取消后 tool_use id={} 缺少配对的 tool_result",
            id
        );
    }
    assert_eq!(
        ai_tool_ids.len(),
        tool_result_ids.len(),
        "取消后所有 tool_use 必须有配对 tool_result"
    );
}

/// 验证混合路径：Ok + ToolRejected + 非 ToolRejected 错误
/// call[0] Ok → 推入 modified_calls
/// call[1] ToolRejected → 独立写入 error tool_result，continue
/// call[2] 非 ToolRejected 错误 → P4 路径，flush modified_calls + flush pending
/// 所有 3 个 tool_use 都应有 tool_result，且无重复写入。
#[tokio::test]
async fn test_mixed_ok_rejected_error_all_tool_results_written() {
    struct MixedResultMiddleware;
    #[async_trait::async_trait]
    impl<S: State> Middleware<S> for MixedResultMiddleware {
        fn name(&self) -> &str {
            "MixedResultMiddleware"
        }
        async fn before_tool(&self, _state: &mut S, tool_call: &ToolCall) -> AgentResult<ToolCall> {
            match tool_call.id.as_str() {
                "id1" => Ok(tool_call.clone()),
                "id2" => Err(AgentError::ToolRejected {
                    tool: tool_call.name.clone(),
                    reason: "用户拒绝".to_string(),
                }),
                _ => Err(AgentError::ToolExecutionFailed {
                    tool: tool_call.name.clone(),
                    reason: "before_tool 错误".to_string(),
                }),
            }
        }
    }

    let agent = ReActAgent::new(make_three_tool_llm("all results received"))
        .max_iterations(5)
        .register_tool(Box::new(make_echo_tool("tool_a")))
        .register_tool(Box::new(make_echo_tool("tool_b")))
        .register_tool(Box::new(make_echo_tool("tool_c")))
        .add_middleware(Box::new(MixedResultMiddleware));

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await;

    assert!(result.is_err(), "混合路径应返回错误，实际: {:?}", result);

    // 延迟写入：before_tool P4 错误路径不写入 AI 消息到 state
    assert_no_orphaned_tool_uses(&state);
    let ai_count = state
        .messages()
        .iter()
        .filter(|m| matches!(m, BaseMessage::Ai { .. }))
        .count();
    assert_eq!(ai_count, 0, "before_tool P4 错误路径不应写入 AI 消息");
}

/// LLM 输出小写 "bash"，工具注册为 PascalCase "Bash"，
/// resolve_tool 通过大小写无关匹配找到工具，不产生 ToolNotFound。
#[tokio::test]
async fn test_tool_name_case_insensitive_fallback() {
    struct EchoTool;
    #[async_trait::async_trait]
    impl BaseTool for EchoTool {
        fn name(&self) -> &str {
            "Bash"
        }
        fn description(&self) -> &str {
            "echo shell"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("shell done".to_string())
        }
    }

    struct LowerCaseBashLLM;
    #[async_trait::async_trait]
    impl ReactLLM for LowerCaseBashLLM {
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
                // LLM 输出小写 "bash"
                Ok(Reasoning::with_tools(
                    "call bash",
                    vec![ToolCall::new("id1", "bash", serde_json::json!({}))],
                ))
            } else {
                Ok(Reasoning::with_answer("done", "shell executed"))
            }
        }
    }

    let agent = ReActAgent::new(LowerCaseBashLLM)
        .max_iterations(5)
        .register_tool(Box::new(EchoTool));

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await;

    assert!(result.is_ok(), "大小写无关匹配应成功，实际: {:?}", result);
    // 验证 tool_result 不是错误
    let has_error_result = state
        .messages()
        .iter()
        .any(|m| matches!(m, BaseMessage::Tool { is_error: true, .. }));
    assert!(!has_error_result, "不应有 ToolNotFound 错误结果");
}

/// LLM 输出 "Task"（别名），工具注册为 "Agent"，
/// resolve_tool 通过语义别名表匹配，不产生 ToolNotFound。
#[tokio::test]
async fn test_tool_name_alias_fallback() {
    struct EchoAgent;
    #[async_trait::async_trait]
    impl BaseTool for EchoAgent {
        fn name(&self) -> &str {
            "Agent"
        }
        fn description(&self) -> &str {
            "echo agent"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("agent done".to_string())
        }
    }

    struct AliasTaskLLM;
    #[async_trait::async_trait]
    impl ReactLLM for AliasTaskLLM {
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
                // LLM 输出别名 "Task"
                Ok(Reasoning::with_tools(
                    "call task",
                    vec![ToolCall::new("id1", "Task", serde_json::json!({}))],
                ))
            } else {
                Ok(Reasoning::with_answer("done", "agent executed"))
            }
        }
    }

    let agent = ReActAgent::new(AliasTaskLLM)
        .max_iterations(5)
        .register_tool(Box::new(EchoAgent));

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await;

    assert!(result.is_ok(), "别名匹配应成功，实际: {:?}", result);
    let has_error_result = state
        .messages()
        .iter()
        .any(|m| matches!(m, BaseMessage::Tool { is_error: true, .. }));
    assert!(!has_error_result, "不应有 ToolNotFound 错误结果");
}

/// 连续 5 次同工具+同错误后注入纠正消息（Human + system-reminder，
/// 不能用 System role 以免污染 frozen system prompt——SC#1）
#[tokio::test]
async fn test_consecutive_failure_injects_correction() {
    struct AlwaysFailRead;
    #[async_trait::async_trait]
    impl BaseTool for AlwaysFailRead {
        fn name(&self) -> &str {
            "Read"
        }
        fn description(&self) -> &str {
            "read"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "properties": { "file_path": { "type": "string" } } })
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Err("The 'file_path' parameter is required for the Read tool.".into())
        }
    }

    struct StubbornLLM;
    #[async_trait::async_trait]
    impl ReactLLM for StubbornLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<crate::llm::types::StreamingContext>,
        ) -> AgentResult<Reasoning> {
            // [SC#1] 纠正消息以 Human + <system-reminder> 注入（非 System role）
            let has_correction = messages.iter().any(|m| {
                matches!(m, BaseMessage::Human { content, .. }
                    if content.text_content().contains("5 consecutive times"))
            });
            if has_correction {
                return Ok(Reasoning::with_answer("done", "I'll stop retrying"));
            }
            Ok(Reasoning::with_tools(
                "retrying",
                vec![ToolCall::new(
                    format!("id_{}", messages.len()),
                    "Read",
                    serde_json::json!({}),
                )],
            ))
        }
    }

    let agent = ReActAgent::new(StubbornLLM)
        .max_iterations(20)
        .register_tool(Box::new(AlwaysFailRead));

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await;

    assert!(result.is_ok(), "Agent 应正常完成，实际: {:?}", result);
    let has_correction = state.messages().iter().any(|m| {
        matches!(m, BaseMessage::Human { content, .. }
            if content.text_content().contains("5 consecutive times"))
    });
    assert!(has_correction, "应注入 Human system-reminder 纠正消息");
    // [SC#1] 防回归：禁止以 System role 注入（会污染 frozen system prompt）
    let no_system_leak = !state.messages().iter().any(|m| {
        matches!(m, BaseMessage::System { content, .. }
            if content.text_content().contains("5 consecutive times"))
    });
    assert!(
        no_system_leak,
        "连续失败纠正消息禁止以 System role 注入（违反 frozen prompt 稳定性）"
    );
}

#[test]
fn test_normalize_params_path_to_file_path() {
    // Arrange: input has "path" but no "file_path"
    let input = serde_json::json!({"path": "/tmp/test.rs", "offset": 10});

    // Act
    let normalized = super::normalize_params(input);

    // Assert: "path" → "file_path", "offset" unchanged
    assert_eq!(normalized["file_path"], "/tmp/test.rs");
    assert!(normalized.get("path").is_none());
    assert_eq!(normalized["offset"], 10);
}

#[test]
fn test_normalize_params_file_path_already_exists() {
    // Arrange: input has both "path" and "file_path" (LLM wrote both)
    let input = serde_json::json!({"path": "/tmp/wrong.rs", "file_path": "/tmp/right.rs"});

    // Act
    let normalized = super::normalize_params(input);

    // Assert: "file_path" unchanged, "path" 不清除（保守策略，不丢数据）
    assert_eq!(normalized["file_path"], "/tmp/right.rs");
    // "path" 保留原样（因为已经有了 file_path，不覆盖）
    assert_eq!(normalized["path"], "/tmp/wrong.rs");
}

#[test]
fn test_normalize_params_no_alias_present() {
    // Arrange: normal Read call with correct parameter names
    let input = serde_json::json!({"file_path": "/tmp/test.rs"});

    // Act
    let normalized = super::normalize_params(input);

    // Assert: no change
    assert_eq!(normalized["file_path"], "/tmp/test.rs");
}

#[test]
fn test_normalize_params_non_object_input() {
    // Arrange: input is a string (edge: unlikely but safe)
    let input = serde_json::Value::String("hello".to_string());

    // Act
    let normalized = super::normalize_params(input);

    // Assert: returned as-is
    assert_eq!(normalized, serde_json::Value::String("hello".to_string()));
}

// ─── A1: Cancel + 部分工具已完成的混合路径 ────────────────────────────

/// 验证执行阶段 cancel：3 个工具并发，fast_tool 在 cancel 到达前已完成，
/// slow_b/slow_c 被 cancel 中断。所有 tool_use 都有配对 tool_result，
/// 已完成的结果不丢失（回归 issue_2026-05-26-ctrl-c-interrupt）。
#[tokio::test]
async fn test_cancel_during_execution_partial_completion() {
    // 快速工具：立即返回成功
    struct FastTool;
    #[async_trait::async_trait]
    impl BaseTool for FastTool {
        fn name(&self) -> &str {
            "fast_tool"
        }
        fn description(&self) -> &str {
            "completes instantly"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("fast done".to_string())
        }
    }

    // 慢速工具：挂起 60s，等待 cancel
    struct SlowTool {
        name_str: &'static str,
    }
    #[async_trait::async_trait]
    impl BaseTool for SlowTool {
        fn name(&self) -> &str {
            self.name_str
        }
        fn description(&self) -> &str {
            "hangs until cancelled"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok("never".to_string())
        }
    }

    struct MixedSpeedLLM;
    #[async_trait::async_trait]
    impl ReactLLM for MixedSpeedLLM {
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
                    "mixed speed",
                    vec![
                        ToolCall::new("id1", "fast_tool", serde_json::json!({})),
                        ToolCall::new("id2", "slow_b", serde_json::json!({})),
                        ToolCall::new("id3", "slow_c", serde_json::json!({})),
                    ],
                ))
            } else {
                Ok(Reasoning::with_answer("done", "all done"))
            }
        }
    }

    let cancel = CancellationToken::new();
    let agent = ReActAgent::new(MixedSpeedLLM)
        .max_iterations(5)
        .register_tool(Box::new(FastTool))
        .register_tool(Box::new(SlowTool { name_str: "slow_b" }))
        .register_tool(Box::new(SlowTool { name_str: "slow_c" }));

    // 200ms 后触发取消：fast_tool 已完成，slow_b/slow_c 还在执行
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        token.cancel();
    });

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, Some(cancel))
        .await;

    // 取消后应返回 Interrupted
    assert!(
        matches!(result, Err(AgentError::Interrupted)),
        "取消后应返回 Interrupted，实际: {:?}",
        result
    );

    // 核心不变量：所有 tool_use 都有配对 tool_result
    assert_no_orphaned_tool_uses(&state);

    // fast_tool 结果应为成功（已完成，非 error）
    let fast_result = state.messages().iter().find_map(|m| {
        if let BaseMessage::Tool {
            tool_call_id,
            is_error,
            content,
            ..
        } = m
        {
            if tool_call_id.as_str() == "id1" {
                Some((*is_error, content.text_content()))
            } else {
                None
            }
        } else {
            None
        }
    });
    assert!(fast_result.is_some(), "fast_tool id=id1 应有 tool_result");
    let (is_error, output) = fast_result.unwrap();
    assert!(
        !is_error,
        "fast_tool 完成结果不应标记为 error，实际输出: {}",
        output
    );
    assert!(
        output.contains("fast done"),
        "fast_tool 应返回正确结果 'fast done'，实际: {}",
        output
    );

    // slow_b 和 slow_c 应有 error tool_result（被中断）
    for id in &["id2", "id3"] {
        let has_error = state.messages().iter().any(|m| {
            matches!(m, BaseMessage::Tool { tool_call_id, is_error: true, .. } if tool_call_id.as_str() == *id)
        });
        assert!(
            has_error,
            "slow tool id={} 被 cancel 中断后应有 error tool_result",
            id
        );
    }
}

// ─── A2: after_tool 错误路径独立测试 ───────────────────────────────────

/// 验证 after_tool 钩子错误路径：工具 invoke 成功（Ok），但 run_after_tool
/// 返回错误 → deferred_error=Some。tool_result 仍写入 state，不丢失结果。
#[tokio::test]
async fn test_after_tool_error_still_writes_result() {
    // 中间件：after_tool 对 tool_b 返回错误
    struct AfterToolFailMiddleware;
    #[async_trait::async_trait]
    impl<S: State> Middleware<S> for AfterToolFailMiddleware {
        fn name(&self) -> &str {
            "AfterToolFail"
        }
        async fn after_tool(
            &self,
            _state: &mut S,
            tool_call: &ToolCall,
            _result: &ToolResult,
        ) -> AgentResult<()> {
            if tool_call.name == "tool_b" {
                Err(AgentError::MiddlewareError {
                    middleware: "AfterToolFail".to_string(),
                    reason: "after_tool 模拟错误".to_string(),
                })
            } else {
                Ok(())
            }
        }
    }

    let agent = ReActAgent::new(make_two_tool_llm("all done"))
        .max_iterations(5)
        .register_tool(Box::new(make_echo_tool("tool_a")))
        .register_tool(Box::new(make_echo_tool("tool_b")))
        .add_middleware(Box::new(AfterToolFailMiddleware));

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await;

    // after_tool 错误导致 dispatch_tools 返回 MiddlewareError
    assert!(
        matches!(&result, Err(AgentError::MiddlewareError { .. })),
        "after_tool 错误应传播为 MiddlewareError，实际: {:?}",
        result
    );

    // 核心不变量：所有 tool_use 都有配对 tool_result
    assert_no_orphaned_tool_uses(&state);

    // tool_a 和 tool_b 的 invoke 都成功了，tool_result 应均为成功
    for id in &["id1", "id2"] {
        let tool_msg = state.messages().iter().find(
            |m| matches!(m, BaseMessage::Tool { tool_call_id, .. } if tool_call_id.as_str() == *id),
        );
        assert!(
            tool_msg.is_some(),
            "tool id={} 应有 tool_result（after_tool 错误不丢失 invoke 成功的结果）",
            id
        );
        if let Some(BaseMessage::Tool {
            is_error, content, ..
        }) = tool_msg
        {
            assert!(
                !is_error,
                "tool id={} invoke 成功，tool_result 不应标记为 error，实际输出: {}",
                id,
                content.text_content()
            );
        }
    }
}

// ─── A3: 全部并发工具都失败 ─────────────────────────────────────────────

/// 验证 3 个工具并发全部失败：所有 invoke 返回 Err。
/// 所有 tool_use 都有 error tool_result，无孤儿 tool_use。
#[tokio::test]
async fn test_all_concurrent_tools_fail() {
    struct FailTool {
        name_str: &'static str,
        err_msg: &'static str,
    }
    #[async_trait::async_trait]
    impl BaseTool for FailTool {
        fn name(&self) -> &str {
            self.name_str
        }
        fn description(&self) -> &str {
            "always fails"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _: serde_json::Value,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Err(self.err_msg.into())
        }
    }

    struct AllFailLLM;
    #[async_trait::async_trait]
    impl ReactLLM for AllFailLLM {
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
                    "call three",
                    vec![
                        ToolCall::new("id_a", "tool_a", serde_json::json!({})),
                        ToolCall::new("id_b", "tool_b", serde_json::json!({})),
                        ToolCall::new("id_c", "tool_c", serde_json::json!({})),
                    ],
                ))
            } else {
                // 3 个 tool_result 都写入 state，都是 error → LLM 应判断后给出最终答案
                Ok(Reasoning::with_answer(
                    "all failed",
                    "all tools failed, stopping",
                ))
            }
        }
    }

    let agent = ReActAgent::new(AllFailLLM)
        .max_iterations(5)
        .register_tool(Box::new(FailTool {
            name_str: "tool_a",
            err_msg: "error_a",
        }))
        .register_tool(Box::new(FailTool {
            name_str: "tool_b",
            err_msg: "error_b",
        }))
        .register_tool(Box::new(FailTool {
            name_str: "tool_c",
            err_msg: "error_c",
        }));

    let mut state = AgentState::new("/tmp");
    let result = agent
        .execute(AgentInput::text("go"), &mut state, None)
        .await;

    // 全部工具失败不阻止 Agent 继续：LLM 在下一轮看到 error tool_result 后给出最终答案
    assert!(
        result.is_ok(),
        "全部工具失败不阻止 Agent 继续，应返回最终答案，实际: {:?}",
        result
    );

    // 核心不变量：所有 tool_use 都有配对 tool_result
    assert_no_orphaned_tool_uses(&state);

    // 3 个 tool_result 都应为 error=true
    let error_results: Vec<_> = state
        .messages()
        .iter()
        .filter_map(|m| {
            if let BaseMessage::Tool {
                tool_call_id,
                is_error,
                ..
            } = m
            {
                Some((tool_call_id.clone(), *is_error))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        error_results.len(),
        3,
        "应有 3 个 tool_result，实际: {:?}",
        error_results
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>()
    );
    for (id, is_error) in &error_results {
        assert!(
            *is_error,
            "tool id={} invoke 失败，tool_result 应标记 is_error=true",
            id.as_str()
        );
    }
}

// ─── P0-1: 连续失败检测 key 稳定化测试 ───────────────────────────────────

#[test]
fn test_classify_failure_kind_covers_common_errors() {
    // not_found 类
    assert_eq!(
        classify_failure_kind("Error: ToolNotFound: foo"),
        "not_found"
    );
    assert_eq!(
        classify_failure_kind("No such file or directory"),
        "not_found"
    );
    assert_eq!(
        classify_failure_kind("Path /tmp/x does not exist"),
        "not_found"
    );
    // timeout 类
    assert_eq!(
        classify_failure_kind("Command timed out after 30s"),
        "timeout"
    );
    assert_eq!(classify_failure_kind("Error: timeout of 5000ms"), "timeout");
    // rejected 类
    assert_eq!(
        classify_failure_kind("Permission denied: operation was blocked"),
        "rejected"
    );
    assert_eq!(classify_failure_kind("Not approved by user"), "rejected");
    // missing_input 类
    assert_eq!(
        classify_failure_kind("Missing required parameter: command"),
        "missing_input"
    );
    // invalid_input 类
    assert_eq!(
        classify_failure_kind("Invalid JSON: malformed input"),
        "invalid_input"
    );
    // exec_failed 类（含 Exit code）
    assert_eq!(
        classify_failure_kind("[stderr]\npanic!\n[Exit code: 134]"),
        "exec_failed"
    );
    // other 类（兜底）
    assert_eq!(classify_failure_kind("some weird error"), "other");
}

#[test]
fn test_make_failure_key_normalizes_dynamic_content() {
    // P0-1 核心：同种错误但 path/UUID 不同，应归一化为同一 key
    let key1 = make_failure_key(
        "Bash",
        "Error: No such file or directory: '/tmp/abc-12345/data.json'",
    );
    let key2 = make_failure_key(
        "Bash",
        "Error: No such file or directory: '/tmp/xyz-98765/config.yaml'",
    );
    assert_eq!(
        key1, key2,
        "同种错误（not_found）不同路径应归一化为同一 key，实际: {key1} vs {key2}"
    );

    // 不同错误类型应分开（避免误合并）
    let key_not_found = make_failure_key("Bash", "No such file or directory");
    let key_timeout = make_failure_key("Bash", "Command timed out");
    assert_ne!(
        key_not_found, key_timeout,
        "不同错误类型应有不同 key，避免误合并"
    );

    // 不同工具应分开
    let key_grep = make_failure_key("Grep", "No such file or directory");
    assert_ne!(key_grep, key_not_found, "不同工具应有不同 key");

    // key 保留 `tool_name:` 前缀（供 retain 重置使用）
    assert!(
        key1.starts_with("Bash:"),
        "key 应保留 `Bash:` 前缀结构，实际: {key1}"
    );
}

#[test]
fn test_consecutive_failure_accumulates_same_kind() {
    // 模拟连续 5 次 not_found 错误，应累计计数
    let mut consecutive: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for _ in 0..5 {
        // 每次 path 不同（动态内容），但错误类型相同
        let key = make_failure_key(
            "Bash",
            "Error: No such file or directory: '/tmp/uuid-XYZ/dynamic-path'",
        );
        let count = consecutive.entry(key).or_insert(0);
        *count += 1;
    }

    // 应有 1 个 entry，count=5（不是 5 个 entry 各 count=1）
    assert_eq!(consecutive.len(), 1, "同种错误应归一化为单个 entry");
    let total: usize = consecutive.values().sum();
    assert_eq!(total, 5, "应累计计数到 5");

    // 重置逻辑：按工具名前缀 retain
    let tool_name = "Bash";
    consecutive.retain(|k, _| !k.starts_with(&format!("{}:", tool_name)));
    assert!(
        consecutive.is_empty(),
        "retain 应清除该工具所有 entry，实际: {:?}",
        consecutive
    );
}
