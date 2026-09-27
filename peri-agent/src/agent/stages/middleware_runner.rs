//! Middleware Runner — v2 stages 与 v1 middleware chain 的桥接层
//!
//! ## 背景
//!
//! middleware 按生命周期获得窄能力接口，AgentContext 是其底层真实适配器。
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
use crate::middleware::capabilities as hook_state;
use crate::middleware::state::MiddlewareState;
use crate::session::tool_catalog::StartupToolUpdate;

/// 从 StageContext 构造 AgentContext
fn make_context_from_stage(ctx: &StageContext) -> AgentContext<'_> {
    AgentContext::from_stage(ctx)
}

/// 启动闸门状态：暂存本次准入的候选工具更新。
///
/// 候选只存在于本次 `run_before_react_start` 的局部 state；middleware 失败、
/// 取消或闸门结束即随 state 丢弃，不落 middleware 内部字段，也不跨 loop 复用。
#[derive(Default)]
struct StartupGateState {
    active_middleware: Option<String>,
    candidate: Option<StartupToolUpdate>,
    candidate_owner: Option<String>,
}

impl StartupGateState {
    /// 候选登记者的名称；无登记时按链级归类。
    fn owner(&self) -> String {
        self.candidate_owner
            .clone()
            .unwrap_or_else(|| "chain".to_string())
    }
}

impl hook_state::StartupState for StartupGateState {
    fn set_active_middleware(&mut self, middleware_name: &str) {
        self.active_middleware = Some(middleware_name.to_string());
    }

    fn stage_startup_tools(&mut self, update: StartupToolUpdate) -> crate::error::AgentResult<()> {
        let owner = self
            .active_middleware
            .clone()
            .unwrap_or_else(|| "chain".to_string());
        if self.candidate.is_some() {
            return Err(crate::error::AgentError::MiddlewareError {
                middleware: owner,
                reason: "System MCP 启动失败：启动闸门已登记候选工具，拒绝重复登记".to_string(),
            });
        }
        self.candidate = Some(update);
        self.candidate_owner = Some(owner);
        Ok(())
    }

    fn take_startup_tools(&mut self) -> Option<StartupToolUpdate> {
        self.candidate.take()
    }
}

/// 调用 middleware chain 的 `before_react_start` 钩子，并把候选原子提交到
/// session tool catalog 的 static base。
///
/// 提交只更新 static base：Reason 边界仍完整走 ARC-TOOLS-001 的
/// `refresh → working map swap → before_reason_catalog → before_model → pin`，
/// 本函数不改写 working map、不替代 Reason boundary。钩子返回 Err 时候选直接
/// 丢弃，目录不变——调用方据此阻止进入 Compact。
pub async fn run_before_react_start(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut gate = StartupGateState::default();
    ctx.runtime
        .middleware_chain
        .run_before_react_start(&mut gate)
        .await?;
    let Some(update) = hook_state::StartupState::take_startup_tools(&mut gate) else {
        return Ok(());
    };
    let committed = ctx
        .runtime
        .tool_catalog
        .replace_static_mcp_tools(update)
        .map_err(|error| crate::error::AgentError::MiddlewareError {
            middleware: gate.owner(),
            reason: format!("System MCP 启动失败：工具目录发布被拒绝，未发布 ready（{error}）"),
        })?;
    tracing::debug!(
        generation = committed.generation,
        tools = committed.tools.len(),
        "startup tool update committed to static base"
    );
    Ok(())
}

// ─── Async 调用辅助 ───────────────────────────────────────────────────────────

/// 调用 middleware chain 的 `before_compact` 钩子（只读，无 drain）
pub async fn run_before_compact(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx
        .runtime
        .middleware_chain
        .run_before_compact(&mut cx)
        .await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `after_compact` 钩子（只读，无 drain）
pub async fn run_after_compact(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx
        .runtime
        .middleware_chain
        .run_after_compact(&mut cx)
        .await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `before_agent` 钩子
pub async fn run_before_agent(
    ctx: &StageContext,
    input_message_ids: &[crate::messages::MessageId],
) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx).with_input_message_ids(input_message_ids);
    let result = ctx.runtime.middleware_chain.run_before_agent(&mut cx).await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    // Sync messages_cache modifications back to transcript.
    // AgentContext::replace_message() updates only existing IDs in the cache;
    // Reason stage reads from the authoritative transcript, so
    // middleware that modify existing messages during initial input preparation
    // must have their changes written through.
    if cx.messages_modified() {
        let mut transcript = ctx.session.transcript.write();
        cx.reconcile_to_transcript(&mut transcript);
    }
    result
}

/// 后续 Receive 的用户输入准备；即使链返回错误，也保留已经完成的替换。
pub async fn run_before_input(
    ctx: &StageContext,
    input_message_ids: &[crate::messages::MessageId],
) -> crate::error::AgentResult<()> {
    if input_message_ids.is_empty() {
        return Ok(());
    }
    let mut cx = make_context_from_stage(ctx).with_input_message_ids(input_message_ids);
    let result = ctx.runtime.middleware_chain.run_before_input(&mut cx).await;
    if cx.messages_modified() {
        cx.reconcile_to_transcript(&mut ctx.session.transcript.write());
    }
    result
}

/// 调用 middleware chain 的 Reason 工具目录刷新钩子。
pub async fn run_before_reason_catalog(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx
        .runtime
        .middleware_chain
        .run_before_reason_catalog(&mut cx)
        .await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// 调用 middleware chain 的 `before_model` 钩子
pub async fn run_before_model(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = ctx.runtime.middleware_chain.run_before_model(&mut cx).await;
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
        .runtime
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
        .runtime
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
        .runtime
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
        .runtime
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
    let result = ctx
        .runtime
        .middleware_chain
        .run_after_agent(&mut cx, output)
        .await;
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
    let result = ctx
        .runtime
        .middleware_chain
        .run_on_error(&mut cx, error)
        .await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "middleware_runner_test.rs"]
mod tests;
