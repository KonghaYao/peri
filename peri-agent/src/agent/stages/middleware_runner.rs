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
use crate::agent::stages::{InvocationContext, ModelTurnFacts, StageContext};
use crate::middleware::capabilities as hook_state;
use crate::middleware::state::MiddlewareState;
use crate::session::tool_catalog::StartupToolUpdate;
use peri_acp_types::mcp_apps::SkippedPostHook;

/// 从 StageContext 构造 AgentContext
fn make_context_from_stage(ctx: &StageContext) -> AgentContext<'_> {
    AgentContext::from_stage(ctx)
}

/// 中断 hook 执行：`cancel` 命中优先于 hook 结果。
///
/// 取消令牌是唯一的中断事实源：阶段级 hook 用 turn 令牌（turn 与 `SessionConfig`
/// 共享），canonical 执行用 [`InvocationContext::cancellation`]（App / Host 入口由
/// 调用方令牌决定）。
async fn run_interruptible_hook<T>(
    cancel: &tokio_util::sync::CancellationToken,
    hook: &'static str,
    future: impl std::future::Future<Output = crate::error::AgentResult<T>>,
) -> crate::error::AgentResult<T> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            tracing::debug!(hook, "middleware hook interrupted");
            Err(crate::error::AgentError::Interrupted)
        }
        result = future => result,
    }
}

pub(crate) async fn run_first_turn_reminders(
    ctx: &StageContext,
) -> crate::error::AgentResult<Vec<String>> {
    let mut cx = make_context_from_stage(ctx);
    run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "first_turn_reminder",
        ctx.runtime
            .middleware_chain
            .run_first_turn_reminders(&mut cx),
    )
    .await
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
    run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "before_react_start",
        ctx.runtime
            .middleware_chain
            .run_before_react_start(&mut gate),
    )
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
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "before_compact",
        ctx.runtime.middleware_chain.run_before_compact(&mut cx),
    )
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
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "after_compact",
        ctx.runtime.middleware_chain.run_after_compact(&mut cx),
    )
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
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "before_agent",
        ctx.runtime.middleware_chain.run_before_agent(&mut cx),
    )
    .await;
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
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "before_input",
        ctx.runtime.middleware_chain.run_before_input(&mut cx),
    )
    .await;
    if cx.messages_modified() {
        cx.reconcile_to_transcript(&mut ctx.session.transcript.write());
    }
    result
}

/// 调用 middleware chain 的 Reason 工具目录刷新钩子。
pub async fn run_before_reason_catalog(ctx: &StageContext) -> crate::error::AgentResult<()> {
    let mut cx = make_context_from_stage(ctx);
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "before_reason_catalog",
        ctx.runtime
            .middleware_chain
            .run_before_reason_catalog(&mut cx),
    )
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
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "before_model",
        ctx.runtime.middleware_chain.run_before_model(&mut cx),
    )
    .await;
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
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "after_model",
        ctx.runtime
            .middleware_chain
            .run_after_model(&mut cx, reasoning),
    )
    .await;
    let rec = cx.drain_recall();
    if !rec.is_empty() {
        ctx.recall_buffer.write().extend(rec);
    }
    result
}

/// Production approval receives the exact targets already bound by dispatch.
///
/// 调用级入口：事实来自 [`InvocationContext`]（会话快照 + turn 事实），不再经过
/// 阶段上下文。取消时逐条返回 `Interrupted`；无 turn 时逐条返回**同一个 typed
/// 不可用错误**（[`AgentError::NotAvailableWithoutTurn`]，带入口名与已知 session
/// 身份：审批不可用而不是放行，也不退化成泛化 `MiddlewareError`）。
/// 无绑定目标的场景（测试）显式传空表。
pub(crate) async fn run_before_bound_tools_batch(
    ctx: &InvocationContext,
    calls: &[crate::agent::react::ToolCall],
    targets: &std::collections::HashMap<String, std::sync::Arc<dyn crate::tools::BaseTool>>,
) -> Vec<crate::error::AgentResult<crate::agent::react::ToolCall>> {
    let interrupted = || {
        calls
            .iter()
            .map(|_| Err(crate::error::AgentError::Interrupted))
            .collect::<Vec<_>>()
    };
    // 审批规则的输入全部是 session 级事实（会话身份 + 归属 + 工具表），因此
    // `turn: None` 下**不早退**：App / Host idle 入口照常执行既有 before-tool 规则
    //（Hook PreToolUse + Permission），归属由 `InteractionAttribution` 承载。
    //
    // 唯一的显式不可用来自**模型来源**（模型批次 / 显式模型调用）：它们按定义处于
    // 模型 turn 内（render 事件纽带、step、turn_generation 都绑定真实 turn），
    // 无 turn 属不成立组合 ⇒ 逐条返回同一个 typed 不可用事实，不伪装成取消、
    // 不静默放行、也不降级为泛化 `MiddlewareError`。
    //
    // `AgentError` 不实现 `Clone`（内含 `anyhow::Error`），因此按同一准入入口与已知
    // session 身份**重建同值错误**：`require_turn` 的唯一错误来源就是这两个字段。
    let turn = match ctx.require_model_turn("before_tools_batch") {
        Ok(turn) => turn,
        Err(_) => {
            return calls
                .iter()
                .map(|_| {
                    Err(crate::error::AgentError::NotAvailableWithoutTurn {
                        entry: "before_tools_batch",
                        session_id: ctx.session_id().map(str::to_owned),
                    })
                })
                .collect();
        }
    };
    let mut state = BoundApprovalState {
        view: InvocationState { context: ctx, turn },
        targets,
    };
    match run_interruptible_hook(ctx.cancellation(), "before_tools_batch", async {
        Ok(ctx
            .middleware_chain()
            .run_before_tools_batch(&mut state, calls.to_vec())
            .await)
    })
    .await
    {
        Ok(results) => results,
        Err(_) => interrupted(),
    }
}

/// 调用期只读视图：会话快照 + turn 事实（可缺），外加队列与 hook 输出端口。
struct InvocationState<'a> {
    context: &'a InvocationContext,
    /// `None` = 无活跃 turn（idle 执行）：规则显式看到缺 turn，不伪造 step。
    turn: Option<&'a ModelTurnFacts>,
}

impl hook_state::StateView for InvocationState<'_> {
    fn execution_binding(&self) -> Option<peri_acp_types::session::ExecutionBinding> {
        self.turn.map(|turn| turn.execution_binding())
    }
    fn session_id(&self) -> Option<&str> {
        self.context.session_id()
    }
    fn interaction_attribution(
        &self,
    ) -> Option<peri_acp_types::interaction::InteractionAttribution> {
        self.context.attribution().cloned()
    }
    fn cwd(&self) -> &str {
        self.context.cwd()
    }
    fn messages(&self) -> &[crate::messages::BaseMessage] {
        self.context.messages()
    }
    /// `None` = 无活跃 turn（idle 执行）：规则必须显式处理，不得把 `0` 当第一步。
    fn current_step(&self) -> Option<usize> {
        self.turn.map(|turn| turn.step())
    }
}

impl hook_state::QueueState for InvocationState<'_> {
    fn v2_queue(&self) -> &crate::session::MessageQueue {
        self.context.queue().queue()
    }

    fn enqueue_v2_message(&self, msg: crate::session::QueuedMessage) {
        self.context.queue().enqueue(msg);
    }
}

/// 显式实现：调用期窄状态不实现 `MiddlewareState`（不提供 `add_message` /
/// `replace_message` 等写能力），因此不走通用适配器的 blanket impl。
impl hook_state::AfterToolState for InvocationState<'_> {}

impl hook_state::HookOutputState for InvocationState<'_> {
    fn enqueue_hook_model_reminder(
        &self,
        reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        self.context
            .queue()
            .enqueue(hook_state::hook_output_message(reminder));
    }

    fn enqueue_hook_client_notice(
        &self,
        reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        self.context
            .queue()
            .enqueue(hook_state::hook_output_message(reminder));
    }

    fn enqueue_session_start_message(
        &self,
        reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        self.context
            .queue()
            .enqueue(hook_state::hook_output_message(reminder));
    }
}

/// 工具审批状态：调用期只读视图 + 已绑定目标身份。
struct BoundApprovalState<'a> {
    view: InvocationState<'a>,
    targets: &'a std::collections::HashMap<String, std::sync::Arc<dyn crate::tools::BaseTool>>,
}

impl hook_state::StateView for BoundApprovalState<'_> {
    fn execution_binding(&self) -> Option<peri_acp_types::session::ExecutionBinding> {
        self.view.execution_binding()
    }
    fn session_id(&self) -> Option<&str> {
        self.view.session_id()
    }
    fn interaction_attribution(
        &self,
    ) -> Option<peri_acp_types::interaction::InteractionAttribution> {
        self.view.interaction_attribution()
    }
    fn cwd(&self) -> &str {
        self.view.cwd()
    }
    fn messages(&self) -> &[crate::messages::BaseMessage] {
        self.view.messages()
    }
    fn current_step(&self) -> Option<usize> {
        self.view.current_step()
    }
}

impl hook_state::HookOutputState for BoundApprovalState<'_> {
    fn enqueue_hook_model_reminder(
        &self,
        reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        self.view.enqueue_hook_model_reminder(reminder);
    }

    fn enqueue_hook_client_notice(
        &self,
        reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        self.view.enqueue_hook_client_notice(reminder);
    }

    fn enqueue_session_start_message(
        &self,
        reminder: peri_acp_types::system_reminder::TrustedSystemReminder,
    ) {
        self.view.enqueue_session_start_message(reminder);
    }
}

impl hook_state::BeforeToolState for BoundApprovalState<'_> {
    fn approval_unavailable_sink(
        &self,
    ) -> Option<std::sync::Arc<crate::agent::stages::ApprovalUnavailableSink>> {
        self.view.context.approval_unavailable_sink()
    }

    fn tool_origin(&self, call_id: &str) -> Option<hook_state::BoundToolOrigin> {
        self.targets
            .get(call_id)
            .map(|tool| hook_state::BoundToolOrigin {
                mcp_server_name: tool.mcp_server_name().map(str::to_owned),
                mcp_tool_name: tool.mcp_tool_name().map(str::to_owned),
                builtin_mcp_instance: tool.builtin_mcp_instance().map(str::to_owned),
            })
    }
}

/// 调用 middleware chain 的 `after_tool` 钩子（调用级）。
///
/// 结果视图 = dispatch 时刻的可见消息快照（提交前视图）；不触碰 transcript，
/// hook 输出经 [`InvocationContext`] 队列与 render 端口投递。
///
/// 无 turn（idle）时**显式记录跳过**（裁决 R2）：事后钩子需要 turn 绑定的事实，
/// 跳过写入执行入口的 outcome 元数据并告警，**不**把已发生副作用的执行改写成失败。
pub(crate) async fn run_after_tool(
    ctx: &InvocationContext,
    call: &crate::agent::react::ToolCall,
    result: &crate::agent::react::ToolResult,
) -> crate::error::AgentResult<()> {
    let Some(turn) = ctx.turn() else {
        ctx.record_skipped_post_hook(SkippedPostHook::AfterToolNotAvailableWithoutTurn);
        return Ok(());
    };
    let mut state = InvocationState {
        context: ctx,
        turn: Some(turn),
    };
    run_interruptible_hook(
        ctx.cancellation(),
        "after_tool",
        ctx.middleware_chain()
            .run_after_tool(&mut state, call, result),
    )
    .await
}

/// 调用 middleware chain 的 `on_error` 钩子（调用级）。
///
/// 与阶段级 [`run_on_error`] 分开：canonical 执行有明确的执行入口（会话快照 +
/// turn 事实 + 入口令牌），阶段级失败（Reason / Compact）没有执行身份，不构造
/// 伪造的执行上下文。无 turn（idle）时同 [`run_after_tool`]：显式记录跳过。
pub(crate) async fn run_invocation_on_error(
    ctx: &InvocationContext,
    error: &crate::error::AgentError,
) -> crate::error::AgentResult<()> {
    let Some(turn) = ctx.turn() else {
        ctx.record_skipped_post_hook(SkippedPostHook::OnErrorNotAvailableWithoutTurn);
        return Ok(());
    };
    let mut state = InvocationState {
        context: ctx,
        turn: Some(turn),
    };
    run_interruptible_hook(
        ctx.cancellation(),
        "on_error",
        ctx.middleware_chain().run_on_error(&mut state, error),
    )
    .await
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
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "after_tools_batch",
        ctx.runtime
            .middleware_chain
            .run_after_tools_batch(&mut cx, results),
    )
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
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "after_agent",
        ctx.runtime
            .middleware_chain
            .run_after_agent(&mut cx, output),
    )
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
    let result = run_interruptible_hook(
        &ctx.session.turn.cancel_token,
        "on_error",
        ctx.runtime.middleware_chain.run_on_error(&mut cx, error),
    )
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

#[cfg(test)]
#[path = "middleware_cancel_test.rs"]
mod cancel_tests;
