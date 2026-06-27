//! Middleware Runner — v2 stages 与 v1 middleware chain 的桥接层
//!
//! ## 背景
//!
//! v1 middleware 通过 `&mut dyn MiddlewareState` 操作状态（messages/context 等）。
//! v2 stages 用 `MessageTranscript`（标记代替删除 + staging 两阶段写入）作为权威。
//!
//! 直接让 MessageTranscript 实现 MiddlewareState 不合适：
//! - MiddlewareState 暴露 `messages_mut() -> &mut Vec<BaseMessage>`，与 staging 语义冲突
//! - compact middleware 需要 drain/extend 整体替换消息
//! - token_tracker / message_queue 等字段在 MessageTranscript 中不存在
//!
//! ## 方案
//!
//! **TranscriptState**：临时工作区，实现 MiddlewareState。
//! 调用 middleware 前：从 transcript 快照构造 TranscriptState
//! middleware 操作 TranscriptState（add_message / messages_mut 等）
//! 调用结束后：把 TranscriptState.messages 整体写回 transcript（保留 ancestor + flags）
//!
//! 这复刻了 AgentState 的工作模式，所有 20 个 middleware 无需改动。

use crate::agent::stages::StageContext;
use crate::agent::state::AgentState;
use crate::messages::BaseMessage;
use crate::session::transcript::MessageFlags;

/// 从 StageContext 构造临时 AgentState（middleware 工作区）
///
/// - 复制 transcript 的 visible_messages 到 state.messages
/// - 共享 turn step / cwd
/// - 复制 token_tracker（point-in-time 快照）
/// - 复制 session_context（session_id / run_id 等）
pub fn snapshot_to_agent_state(ctx: &StageContext) -> AgentState {
    let mut state = AgentState::new(ctx.cwd());

    // 拷贝可见消息
    let visible: Vec<BaseMessage> = ctx
        .transcript
        .read()
        .visible_messages()
        .into_iter()
        .cloned()
        .collect();
    *state.messages_mut() = visible;

    // 共享 step
    state.set_current_step(ctx.turn.current_step());

    // 共享 session_context
    {
        let session_ctx = ctx.session_context.read();
        for (k, v) in session_ctx.iter() {
            state.set_context(k.clone(), v.clone());
        }
    }

    // 共享 v2 MessageQueue：middleware（goal steering / stop-hook feedback）push
    // 的消息直接进入 session 级收件箱，由 Receive / End 阶段统一消费。
    // MessageQueue 内部 Arc 共享，clone 不复制底层数据。
    state.v2_queue = ctx.queue.clone();

    state
}

/// 把 AgentState 的 messages 整体写回 transcript
///
/// - 使用 rebuild 替换 transcript 的 entries（保留 ancestor_len / persistence）
/// - 全部新消息使用 MessageFlags::default()（compact 后由 middleware 决定后续标记）
/// - 不保留旧消息的 truncated/excluded 标记（compact 已生成全新摘要 + re_inject）
/// - **recall 累加**：middleware 把召回提示（如 ToolSearch 更新）推到临时
///   state 的 recall 中，本函数 drain 后追加到
///   [`StageContext::recall_buffer`]，循环结束后由 executor 统一取出。
pub fn restore_from_agent_state(ctx: &StageContext, mut state: AgentState) {
    // 把 middleware hook 期间新增的 recall drain 到共享缓冲区。
    // 必须在 into_messages 消费 state 之前完成。
    let new_recalls = state.drain_recall();
    if !new_recalls.is_empty() {
        ctx.recall_buffer.write().extend(new_recalls);
    }

    let new_messages = state.into_messages();

    // 把 messages 转为 (BaseMessage, MessageFlags) 对，全部默认 flags
    let entries: Vec<(BaseMessage, MessageFlags)> = new_messages
        .into_iter()
        .map(|m| (m, MessageFlags::default()))
        .collect();

    let mut transcript = ctx.transcript.write();
    // 取出旧 transcript，rebuild 后写回
    let old = std::mem::take(&mut *transcript);
    *transcript = old.rebuild(entries);
}

/// 在 middleware 工作区内执行闭包（同步版本）
///
/// 构造临时 AgentState → 调用闭包 → 写回 transcript。
pub fn run_with_state<R>(ctx: &StageContext, f: impl FnOnce(&mut AgentState) -> R) -> R {
    let mut state = snapshot_to_agent_state(ctx);
    let result = f(&mut state);
    restore_from_agent_state(ctx, state);
    result
}

// ─── Async 调用辅助：把 MiddlewareState 显式传给 chain ───────────────────

/// 调用 middleware chain 的 `before_compact` 钩子
///
/// Compact 前置钩子。在 compact 执行前调用，中间件可在此监听/干预 compact 生命周期。
/// 钩子失败不影响 compact 主流程——调用方自行 warn 并继续。
pub async fn run_before_compact(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut state = snapshot_to_agent_state(ctx);
    ctx.middleware_chain.run_before_compact(&mut state).await
}

/// 调用 middleware chain 的 `after_compact` 钩子
///
/// Compact 后置钩子。在 compact 完成后调用（含成功和降级跳过）。
/// 钩子失败不影响 compact 主流程——调用方自行 warn 并继续。
pub async fn run_after_compact(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut state = snapshot_to_agent_state(ctx);
    ctx.middleware_chain.run_after_compact(&mut state).await
}

/// 调用 middleware chain 的 `before_agent` 钩子
pub async fn run_before_agent(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut state = snapshot_to_agent_state(ctx);
    let result = ctx.middleware_chain.run_before_agent(&mut state).await;
    restore_from_agent_state(ctx, state);
    result
}

/// 调用 middleware chain 的 `before_model` 钩子
pub async fn run_before_model(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut state = snapshot_to_agent_state(ctx);
    let result = ctx.middleware_chain.run_before_model(&mut state).await;
    restore_from_agent_state(ctx, state);
    result
}

/// 调用 middleware chain 的 `after_model` 钩子
pub async fn run_after_model(
    ctx: &StageContext,
    reasoning: &crate::agent::react::Reasoning,
) -> crate::error::AgentResult<()> {
    let mut state = snapshot_to_agent_state(ctx);
    let result = ctx
        .middleware_chain
        .run_after_model(&mut state, reasoning)
        .await;
    restore_from_agent_state(ctx, state);
    result
}

/// 调用 middleware chain 的 `before_tools_batch` 钩子（批量审批）
pub async fn run_before_tools_batch(
    ctx: &StageContext,
    calls: &[crate::agent::react::ToolCall],
) -> Vec<crate::error::AgentResult<crate::agent::react::ToolCall>> {
    let mut state = snapshot_to_agent_state(ctx);
    let result = ctx
        .middleware_chain
        .run_before_tools_batch(&mut state, calls.to_vec())
        .await;
    restore_from_agent_state(ctx, state);
    result
}

/// 调用 middleware chain 的 `after_tool` 钩子
pub async fn run_after_tool(
    ctx: &StageContext,
    call: &crate::agent::react::ToolCall,
    result: &crate::agent::react::ToolResult,
) -> crate::error::AgentResult<()> {
    let mut state = snapshot_to_agent_state(ctx);
    let res = ctx
        .middleware_chain
        .run_after_tool(&mut state, call, result)
        .await;
    restore_from_agent_state(ctx, state);
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
    let mut state = snapshot_to_agent_state(ctx);
    let result = ctx
        .middleware_chain
        .run_after_tools_batch(&mut state, results)
        .await;
    restore_from_agent_state(ctx, state);
    result
}

/// 调用 middleware chain 的 `after_agent` 钩子（可能修改 output）
pub async fn run_after_agent(
    ctx: &StageContext,
    output: crate::agent::react::AgentOutput,
) -> crate::error::AgentResult<crate::agent::react::AgentOutput> {
    let mut state = snapshot_to_agent_state(ctx);
    let result = ctx
        .middleware_chain
        .run_after_agent(&mut state, output)
        .await;
    restore_from_agent_state(ctx, state);
    result
}

/// 调用 middleware chain 的 `on_error` 钩子
pub async fn run_on_error(
    ctx: &StageContext,
    error: &crate::error::AgentError,
) -> crate::error::AgentResult<()> {
    let mut state = snapshot_to_agent_state(ctx);
    let result = ctx.middleware_chain.run_on_error(&mut state, error).await;
    restore_from_agent_state(ctx, state);
    result
}

// ─── 测试辅助：避免 unused 警告 ──────────────────────────────────────────────

#[allow(dead_code)]
fn _silence_unused() {
    // 占位：原 PhantomData 引用已随 import 清理移除
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
    fn test_snapshot_copies_visible_messages() {
        let ctx = make_context();
        ctx.transcript
            .write()
            .append(BaseMessage::human(MessageContent::text("hello")));

        let state = snapshot_to_agent_state(&ctx);
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].content(), "hello");
    }

    #[test]
    fn test_restore_replaces_transcript_messages() {
        let ctx = make_context();
        ctx.transcript
            .write()
            .append(BaseMessage::human(MessageContent::text("old")));

        let mut state = snapshot_to_agent_state(&ctx);
        state.add_message(BaseMessage::human(MessageContent::text("new")));
        restore_from_agent_state(&ctx, state);

        let transcript = ctx.transcript.read();
        assert_eq!(transcript.len(), 2, "old + new 都应保留");
        assert_eq!(transcript.entries()[0].message.content(), "old");
        assert_eq!(transcript.entries()[1].message.content(), "new");
    }

    #[test]
    fn test_snapshot_excluded_messages_filtered() {
        let ctx = make_context();
        let id = ctx
            .transcript
            .write()
            .append(BaseMessage::human(MessageContent::text("excluded")));
        ctx.transcript.write().set_excluded(id, true);
        ctx.transcript
            .write()
            .append(BaseMessage::human(MessageContent::text("visible")));

        let state = snapshot_to_agent_state(&ctx);
        assert_eq!(
            state.messages().len(),
            1,
            "excluded 消息不应进入 middleware 视野"
        );
        assert_eq!(state.messages()[0].content(), "visible");
    }

    // ── recall_buffer：跨 middleware hook 累积 recall ──────────────────────────

    /// restore_from_agent_state 必须把 middleware hook 期间 push 的 recall
    /// drain 到 StageContext.recall_buffer，避免 v2 路径下 recall 丢失。
    #[test]
    fn test_restore_drains_recall_to_buffer() {
        let ctx = make_context();
        // 模拟 middleware hook：构造临时 state，push_recall 后 restore
        let mut state = snapshot_to_agent_state(&ctx);
        state.push_recall("[ToolSearch] Deferred tools updated: 5 tools".to_string());
        state.push_recall("[MCP] Sentry connected".to_string());
        restore_from_agent_state(&ctx, state);
        // 验证 recall_buffer 累积了 2 条
        let recalls = ctx.recall_buffer.read();
        assert_eq!(
            recalls.len(),
            2,
            "recall_buffer 应累积 middleware push 的 2 条 recall"
        );
        assert_eq!(recalls[0], "[ToolSearch] Deferred tools updated: 5 tools");
        assert_eq!(recalls[1], "[MCP] Sentry connected");
    }

    /// 多次 middleware hook（before_agent / before_model 等）的 recall 应全部累积。
    #[test]
    fn test_restore_accumulates_recall_across_hooks() {
        let ctx = make_context();
        // 第一次 hook：before_agent
        {
            let mut state = snapshot_to_agent_state(&ctx);
            state.push_recall("recall-from-before-agent".to_string());
            restore_from_agent_state(&ctx, state);
        }
        // 第二次 hook：before_model
        {
            let mut state = snapshot_to_agent_state(&ctx);
            state.push_recall("recall-from-before-model".to_string());
            restore_from_agent_state(&ctx, state);
        }
        // 验证两条 recall 都在 buffer
        let recalls = ctx.recall_buffer.read();
        assert_eq!(
            recalls.len(),
            2,
            "跨 hook 累积的 recall 应全部保留在 buffer"
        );
        assert_eq!(recalls[0], "recall-from-before-agent");
        assert_eq!(recalls[1], "recall-from-before-model");
    }

    /// 无 recall push 时，buffer 保持空（不引入假数据）。
    #[test]
    fn test_restore_no_recall_keeps_buffer_empty() {
        let ctx = make_context();
        let state = snapshot_to_agent_state(&ctx);
        restore_from_agent_state(&ctx, state);
        assert!(
            ctx.recall_buffer.read().is_empty(),
            "无 recall push 时 buffer 应为空"
        );
    }
}
