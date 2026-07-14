//! Middleware Runner — v2 stages 与 v1 middleware chain 的桥接层
//!
//! ## 背景
//!
//! v1 middleware 通过 `&mut dyn MiddlewareState` 操作状态（messages/context 等）。
//! v2 stages 用 `MessageTranscript`（标记代替删除 + staging 两阶段写入）作为权威。
//!
//! ## 方案
//!
//! **AgentContext**：`StageContext` 的薄封装，实现 `MiddlewareState`。
//! 每次 middleware hook 调用时从 StageContext 构造 AgentContext，
//! middleware 操作 AgentContext（add_message 双写 transcript + cache，push_recall 累积到内部缓冲区），
//! 调用结束后由 runner drain recall 到 `ctx.recall_buffer`。
//!
//! 与旧方案（snapshot→call→restore）的关键区别：
//! - 不再需要 `restore_from_agent_state.rebuild()`（消除 O(n) 全量 entries 重建）
//! - add_message 直接双写 transcript + cache，不再依赖 restore 回写
//! - messages_cache 保持为一次性快照（后续 `messages()` 零开销引用）

use crate::agent::agent_context::AgentContext;
use crate::agent::stages::StageContext;
use crate::middleware::state::MiddlewareState;

/// 从 StageContext 构造 AgentContext
fn make_context_from_stage(ctx: &StageContext) -> AgentContext<'_> {
    AgentContext::from_stage(ctx)
}

// ─── Async 调用辅助 ───────────────────────────────────────────────────────────

/// 调用 middleware chain 的 `before_compact` 钩子（只读，无 drain）
pub async fn run_before_compact(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx.middleware_chain.run_before_compact(&mut cx).await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `after_compact` 钩子（只读，无 drain）
pub async fn run_after_compact(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx.middleware_chain.run_after_compact(&mut cx).await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `before_agent` 钩子
pub async fn run_before_agent(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx.middleware_chain.run_before_agent(&mut cx).await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `before_model` 钩子
pub async fn run_before_model(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx.middleware_chain.run_before_model(&mut cx).await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `after_model` 钩子
pub async fn run_after_model(
    ctx: &StageContext,
    reasoning: &crate::agent::react::Reasoning,
) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx
        .middleware_chain
        .run_after_model(&mut cx, reasoning)
        .await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `before_tools_batch` 钩子（批量审批）
pub async fn run_before_tools_batch(
    ctx: &StageContext,
    calls: &[crate::agent::react::ToolCall],
) -> Vec<crate::error::AgentResult<crate::agent::react::ToolCall>> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx
        .middleware_chain
        .run_before_tools_batch(&mut cx, calls.to_vec())
        .await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `after_tool` 钩子
pub async fn run_after_tool(
    ctx: &StageContext,
    call: &crate::agent::react::ToolCall,
    result: &crate::agent::react::ToolResult,
) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let res = ctx
        .middleware_chain
        .run_after_tool(&mut cx, call, result)
        .await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    res
}

/// 调用 middleware chain 的 `after_tools_batch` 钩子
pub async fn run_after_tools_batch(
    ctx: &StageContext,
    results: &[(
        crate::agent::react::ToolCall,
        crate::agent::react::ToolResult,
    )],
) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx
        .middleware_chain
        .run_after_tools_batch(&mut cx, results)
        .await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `after_agent` 钩子（可能修改 output）
pub async fn run_after_agent(
    ctx: &StageContext,
    output: crate::agent::react::AgentOutput,
) -> crate::error::AgentResult<crate::agent::react::AgentOutput> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx.middleware_chain.run_after_agent(&mut cx, output).await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `on_error` 钩子
pub async fn run_on_error(
    ctx: &StageContext,
    error: &crate::error::AgentError,
) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx.middleware_chain.run_on_error(&mut cx, error).await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::stages::StageContext;
    use crate::messages::{BaseMessage, MessageContent};
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

    #[test]
    fn test_agent_context_add_message_dual_writes() {
        let ctx = make_context();
        ctx.transcript
            .write()
            .append(BaseMessage::human(MessageContent::text("old")));

        let mut cx = make_context_from_stage(&ctx);
        cx.add_message(BaseMessage::human(MessageContent::text("new")));

        assert_eq!(cx.messages().len(), 2);
        let transcript = ctx.transcript.read();
        assert_eq!(transcript.len(), 2);
    }

    #[test]
    fn test_drain_recall_to_buffer() {
        let ctx = make_context();
        {
            let mut cx = make_context_from_stage(&ctx);
            cx.push_recall("recall-1".to_string());
            cx.push_recall("recall-2".to_string());
            let drained = cx.drain_recall();
            assert_eq!(drained.len(), 2);
            assert!(cx.drain_recall().is_empty());
            // 手动 drain 到 ctx.recall_buffer
            ctx.recall_buffer.write().extend(drained);
        }
        let recalls = ctx.recall_buffer.read();
        assert_eq!(recalls.len(), 2);
        assert_eq!(recalls[0], "recall-1");
        assert_eq!(recalls[1], "recall-2");
    }

    #[test]
    fn test_recall_accumulates_across_hooks() {
        let ctx = make_context();
        {
            let mut cx = make_context_from_stage(&ctx);
            cx.push_recall("hook-1".to_string());
            let rec = cx.drain_recall();
            ctx.recall_buffer.write().extend(rec);
        }
        {
            let mut cx = make_context_from_stage(&ctx);
            cx.push_recall("hook-2".to_string());
            let rec = cx.drain_recall();
            ctx.recall_buffer.write().extend(rec);
        }
        let recalls = ctx.recall_buffer.read();
        assert_eq!(recalls.len(), 2);
        assert_eq!(recalls[0], "hook-1");
        assert_eq!(recalls[1], "hook-2");
    }

    #[test]
    fn test_no_recall_keeps_buffer_empty() {
        let ctx = make_context();
        let mut cx = make_context_from_stage(&ctx);
        let drained = cx.drain_recall();
        assert!(drained.is_empty());
    }
}
