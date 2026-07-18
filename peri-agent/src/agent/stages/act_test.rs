//! 从 act.rs 分离的测试模块
use super::*;
use crate::agent::react::{Reasoning, ToolCall};
use crate::agent::stages::StageContext;
use crate::session::store::FrozenContext;
use crate::session::Session;
use std::sync::Arc;

fn make_context() -> StageContext {
    let cwd: Arc<str> = Arc::from("/tmp/test");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    StageContext::new(turn, session.transcript(), session.queue().clone())
}

#[tokio::test]
async fn test_act_no_tool_calls_writes_answer() {
    let ctx = make_context();
    let reasoning = Reasoning::with_answer("thinking", "final answer");
    let input = ActInput {
        context: ctx.clone(),
        reasoning,
    };
    let output = run_act(input).await.unwrap();
    assert!(!output.has_tool_calls);
    assert_eq!(output.final_answer.as_deref(), Some("final answer"));

    // transcript 应包含 final_answer 消息
    let messages: Vec<_> = {
        let guard = ctx.session.transcript.read();
        guard.visible_messages().into_iter().cloned().collect()
    };
    assert!(
        messages.iter().any(|m| m.content() == "final answer"),
        "transcript 应写入最终回答"
    );
}

#[tokio::test]
async fn test_act_with_tool_calls_dispatches() {
    let ctx = make_context();
    // 工具不存在，dispatch 会返回 not_found 错误结果
    let tool_call = ToolCall::new("call_1", "Read", serde_json::json!({"path": "/tmp"}));
    let reasoning = Reasoning::with_tools("need to read file", vec![tool_call]);
    let input = ActInput {
        context: ctx.clone(),
        reasoning,
    };
    let output = run_act(input).await.unwrap();
    assert!(output.has_tool_calls, "有 tool_calls 时应标记");
    assert!(output.final_answer.is_none());

    // transcript 应包含 AI 消息 + tool_result
    let messages: Vec<_> = {
        let guard = ctx.session.transcript.read();
        guard.visible_messages().into_iter().cloned().collect()
    };
    assert!(
        messages.len() >= 2,
        "应有 AI 消息 + tool_result，实际 {} 条",
        messages.len()
    );
}
