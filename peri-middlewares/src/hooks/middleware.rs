//! Plugin hook middleware — fires registered hooks at lifecycle events.
//!
//! 本文件是 Facade：保留 [`HookMiddleware`] struct + `Middleware` trait 实现
//! 作为唯一 public 类型（API 兼容），实际职责委托给 `hooks/` 目录下同级子模块：
//!
//! - `dispatcher`：核心 hook 分发引擎（fire_event 内部循环 + standalone 路径统一）
//! - `input_builder`：`HookInput` 字面量构造集中收口
//! - `action_resolver`：`HookAction` → `AgentResult` / `ToolCall` 归约（消除 5 处重复 match）
//! - `permission_gate`：PermissionRequest 双条件门控
//! - `stop_block_guard`：Stop Block 连续次数状态机（上限 8）
//! - `once_tracker`：一次性 hook 状态跟踪
//!
//! [TRAP] collect_tool_results 延迟写入不变量：dispatcher 和 action_resolver
//! 不写 state（仅 `after_agent` 的 stop_block 写 state）。

// 兼容旧调用点（`crate::hooks::middleware::fire_standalone_lifecycle_hooks`）：
// 函数已迁移到 `dispatcher.rs`，此处保留 pub use 以维持 ABI。
pub use crate::hooks::dispatcher::fire_standalone_lifecycle_hooks;

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::{
    agent::{
        react::{AgentOutput, ReactLLM, ToolCall, ToolResult},
        state::State,
    },
    error::{AgentError, AgentResult},
    messages::BaseMessage,
    middleware::Middleware,
};

use crate::hitl::SharedPermissionMode;
// HookType 仅 `middleware_test.rs` 通过 `use super::*` 使用。保留以维持测试不变。
#[allow(unused_imports)]
use crate::hooks::{
    action_resolver,
    dispatcher::HookDispatcher,
    input_builder,
    once_tracker::OnceTracker,
    permission_gate,
    stop_block_guard::{format_stop_block_feedback, GuardDecision, StopBlockGuard},
    types::{HookAction, HookEvent, HookInput, HookType, RegisteredHook},
};

/// Plugin hook middleware — fires registered hooks at lifecycle events.
pub struct HookMiddleware {
    /// 核心分发引擎（fire_event 内部循环 + once 跟踪）。
    dispatcher: HookDispatcher,
    /// 共享上下文字段，构造期确定后只读。
    cwd: String,
    session_id: String,
    transcript_path: String,
    /// 共享权限模式（运行时可变，Shift+Tab 切换）。
    /// PermissionRequest 仅在权限对话框即将展示时触发。
    permission_mode: Arc<SharedPermissionMode>,
    current_model: String,
    /// SessionStart 的 source 值（"startup"/"resume"/"clear"/"compact"）。
    /// None 表示不触发 SessionStart。
    session_start_source: Option<String>,
    /// 判断工具是否需要用户审批。用于 PermissionRequest hook 门控。
    /// 默认使用 [`crate::hitl::default_requires_approval`]，
    /// 可通过 `with_requires_approval` 覆盖。
    requires_approval: fn(&str) -> bool,
    /// Stop hook block 连续次数计数器（最多 8 次，超过后忽略）
    stop_block_guard: Arc<StopBlockGuard>,
}

impl HookMiddleware {
    pub fn new(
        registered_hooks: Vec<RegisteredHook>,
        llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
        transcript_path: impl Into<String>,
        permission_mode: Arc<SharedPermissionMode>,
        current_model: impl Into<String>,
    ) -> Self {
        Self::with_session_start(
            registered_hooks,
            llm_factory,
            cwd,
            session_id,
            transcript_path,
            permission_mode,
            current_model,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_session_start(
        registered_hooks: Vec<RegisteredHook>,
        llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
        transcript_path: impl Into<String>,
        permission_mode: Arc<SharedPermissionMode>,
        current_model: impl Into<String>,
        session_start_source: Option<String>,
    ) -> Self {
        let mut map: HashMap<HookEvent, Vec<RegisteredHook>> = HashMap::new();
        for hook in registered_hooks {
            map.entry(hook.event.clone()).or_default().push(hook);
        }
        let event_count = map.len();
        let total_hooks: usize = map.values().map(|v| v.len()).sum();
        tracing::info!(
            total_hooks,
            event_count,
            session_start = session_start_source.is_some(),
            "HookMiddleware created with registered hooks"
        );

        let once_tracker = Arc::new(OnceTracker::new());
        let stop_block_guard = Arc::new(StopBlockGuard::new());

        let cwd_owned: String = cwd.into();
        Self {
            dispatcher: HookDispatcher::new(
                Arc::new(RwLock::new(map)),
                llm_factory,
                once_tracker,
                cwd_owned.clone(),
            ),
            cwd: cwd_owned,
            session_id: session_id.into(),
            transcript_path: transcript_path.into(),
            permission_mode,
            current_model: current_model.into(),
            session_start_source,
            requires_approval: crate::hitl::default_requires_approval,
            stop_block_guard,
        }
    }

    // -----------------------------------------------------------------------
    // fire_event — Facade 委托给 dispatcher，保留 pub(crate) 接口供测试与
    // middleware trait 方法调用。
    // -----------------------------------------------------------------------

    /// 触发 hook 事件，返回归约后的最终 action。
    ///
    /// 保留在 facade 层是因为 `middleware_test.rs` 直接通过 `mw.fire_event(...)`
    /// 调用，且 middleware trait 方法（before_agent/before_tool/...）也通过它
    /// 委托到 dispatcher。
    pub(crate) async fn fire_event(
        &self,
        event: HookEvent,
        input: &HookInput,
        tool_name: Option<&str>,
        tool_input: Option<&serde_json::Value>,
    ) -> HookAction {
        self.dispatcher
            .fire_event(event, input, tool_name, tool_input)
            .await
    }

    /// 在一批并行工具调用全部完成后触发 PostToolBatch hook。
    /// 由 dispatch_tools 在所有 tool_result 写入后调用。
    pub async fn fire_post_tool_batch<S: State>(&self, state: &mut S) -> AgentResult<()> {
        let prompt_text = state
            .messages()
            .iter()
            .rev()
            .find(|m| matches!(m, BaseMessage::Human { .. }))
            .map(|m| m.content())
            .unwrap_or_default();

        let input = input_builder::post_tool_batch(
            &self.session_id,
            &self.transcript_path,
            &self.cwd,
            &format!("{:?}", self.permission_mode.load()),
            &self.current_model,
            &prompt_text,
            state.messages().len(),
        );

        let action = self
            .fire_event(HookEvent::PostToolBatch, &input, None, None)
            .await;

        action_resolver::resolve_post_tool_batch_action(&action)
    }
}

#[async_trait]
impl<S: State> Middleware<S> for HookMiddleware {
    fn name(&self) -> &str {
        "HookMiddleware"
    }

    async fn before_agent(&self, state: &mut S) -> AgentResult<()> {
        // Extract the latest human message as prompt text
        let prompt = state
            .messages()
            .iter()
            .rev()
            .find(|m| matches!(m, BaseMessage::Human { .. }))
            .map(|m| m.content())
            .unwrap_or_default();

        // SessionStart: only when session_start_source is Some
        if let Some(ref source) = self.session_start_source {
            let input = HookInput::session_start(
                &self.session_id,
                &self.transcript_path,
                &self.cwd,
                source,
                &self.current_model,
            );
            let action = self
                .fire_event(HookEvent::SessionStart, &input, None, None)
                .await;
            action_resolver::resolve_action_to_result(
                &action,
                "SessionStart",
                "SessionStart hook prevented continuation",
            )?;
            match &action {
                HookAction::SystemMessage { message } => {
                    tracing::info!("SessionStart hook system message: {}", message);
                }
                HookAction::AdditionalContext { context } => {
                    tracing::info!("SessionStart hook additional context: {}", context);
                }
                HookAction::InitialUserMessage { message } => {
                    tracing::info!("SessionStart hook initial user message: {}", message);
                }
                _ => {}
            }
        }

        // UserPromptSubmit: on every user prompt
        let input = HookInput::user_prompt_submit(
            &self.session_id,
            &self.transcript_path,
            &self.cwd,
            &prompt,
        );
        let action = self
            .fire_event(HookEvent::UserPromptSubmit, &input, None, None)
            .await;

        action_resolver::resolve_action_to_result(
            &action,
            "UserPromptSubmit",
            "Hook prevented continuation",
        )?;

        Ok(())
    }

    async fn before_tool(&self, _state: &mut S, tool_call: &ToolCall) -> AgentResult<ToolCall> {
        let permission_mode_str = format!("{:?}", self.permission_mode.load());
        let input = HookInput::tool_call(
            &self.session_id,
            &self.transcript_path,
            &self.cwd,
            &permission_mode_str,
            &tool_call.name,
            &tool_call.input,
            &tool_call.id,
        );

        // Fire PreToolUse
        let action = self
            .fire_event(
                HookEvent::PreToolUse,
                &input,
                Some(&tool_call.name),
                Some(&tool_call.input),
            )
            .await;

        // 原实现的 `_ => {}`：只有 Block / PreventContinuation / ModifyInput 会
        // 提前 return，其余（Allow / Notification / SystemMessage / ...）继续走
        // PermissionRequest 门控。
        match &action {
            HookAction::Block { .. }
            | HookAction::PreventContinuation { .. }
            | HookAction::ModifyInput { .. } => {
                // resolve_action_to_toolcall: Block/Prevent → Err, ModifyInput → Ok(new ToolCall)
                return action_resolver::resolve_action_to_toolcall(
                    &action,
                    tool_call,
                    "Hook prevented continuation",
                );
            }
            _ => {}
        }

        // PermissionRequest 门控：仅对敏感工具 + 权限对话框即将展示时触发。
        //
        // Claude Code 行为：PermissionRequest 仅在权限对话框即将展示给用户时触发。
        // Bypass 不展示对话框，因此不触发。
        //
        // 使用 hitl::default_requires_approval 判断工具是否需要审批（Bash/Write/Edit/Agent/
        // mcp__*/WebFetch/WebSearch 等）。非敏感工具（Read/Glob/Grep 等）不触发。
        let should_fire = permission_gate::should_fire_permission_request(
            self.permission_mode.load(),
            &tool_call.name,
            self.requires_approval,
        );

        if should_fire {
            let action = self
                .fire_event(
                    HookEvent::PermissionRequest,
                    &input,
                    Some(&tool_call.name),
                    Some(&tool_call.input),
                )
                .await;

            // Fire Notification (agent is waiting for user permission)
            self.fire_event(
                HookEvent::Notification,
                &input,
                Some(&tool_call.name),
                Some(&tool_call.input),
            )
            .await;

            return action_resolver::resolve_action_to_toolcall(
                &action,
                tool_call,
                "Hook prevented continuation",
            );
        }

        Ok(tool_call.clone())
    }

    async fn after_tool(
        &self,
        _state: &mut S,
        tool_call: &ToolCall,
        result: &ToolResult,
    ) -> AgentResult<()> {
        let event = if result.is_error {
            HookEvent::PostToolUseFailure
        } else {
            HookEvent::PostToolUse
        };

        let permission_mode_str = format!("{:?}", self.permission_mode.load());
        let input = HookInput::tool_result(
            &self.session_id,
            &self.transcript_path,
            &self.cwd,
            &permission_mode_str,
            &tool_call.name,
            &tool_call.input,
            &action_resolver::tool_output_to_json(result),
            result.is_error,
        );

        let _action = self
            .fire_event(event, &input, Some(&tool_call.name), Some(&tool_call.input))
            .await;

        Ok(())
    }

    async fn after_tools_batch(
        &self,
        state: &mut S,
        _results: &[(ToolCall, ToolResult)],
    ) -> AgentResult<()> {
        self.fire_post_tool_batch(state).await
    }

    async fn after_agent(&self, state: &mut S, output: &AgentOutput) -> AgentResult<AgentOutput> {
        let input = input_builder::stop(
            &self.session_id,
            &self.transcript_path,
            &self.cwd,
            &format!("{:?}", self.permission_mode.load()),
            &self.current_model,
            output,
        );

        let action = self.fire_event(HookEvent::Stop, &input, None, None).await;

        match &action {
            HookAction::Block { reason } => match self.stop_block_guard.on_block(reason) {
                GuardDecision::ForceFinish => {
                    return Ok(output.clone());
                }
                GuardDecision::Block { count, reason } => {
                    // [TRAP] 必须用 Human + <system-reminder> 注入，禁止 BaseMessage::system。
                    // System 消息会被 anthropic/openai invoke hoist 到 system prompt 顶部，
                    // 违反 frozen_system_prompt 稳定性（第一优先级）。
                    // （与 goal_middleware.rs / compact_middleware.rs 注入路径一致）
                    state.add_message(BaseMessage::human(format_stop_block_feedback(
                        &reason, count,
                    )));
                    let mut output = output.clone();
                    output.block_continue = Some(reason);
                    return Ok(output);
                }
                GuardDecision::Pass => {}
            },
            HookAction::PreventContinuation { stop_reason } => {
                return Err(AgentError::ToolRejected {
                    tool: "Stop".to_string(),
                    reason: stop_reason
                        .clone()
                        .unwrap_or_else(|| "Stop hook prevented continuation".to_string()),
                });
            }
            _ => {
                // 非 block 时重置计数器
                self.stop_block_guard.on_non_block();
            }
        }

        // Fire Notification (agent done, waiting for user input)
        self.fire_event(HookEvent::Notification, &input, None, None)
            .await;

        Ok(output.clone())
    }

    async fn on_error(&self, _state: &mut S, error: &AgentError) -> AgentResult<()> {
        // StopFailure 仅在 API/LLM 调用失败时触发，
        // 跳过 Interrupted、MaxIterationsExceeded、ToolRejected 等非 API 错误。
        let should_fire = matches!(
            error,
            AgentError::LlmError(_)
                | AgentError::LlmHttpError { .. }
                | AgentError::MiddlewareError { .. }
        );

        if !should_fire {
            return Ok(());
        }

        // 当 agent 因 API/LLM 错误退出时触发 StopFailure hook。
        // 非 API 错误（Interrupted / MaxIterationsExceeded / ToolRejected 等）
        // 已在 guard 中过滤。
        let error_description = format!("{:?}", error);
        let input = input_builder::stop_failure(
            &self.session_id,
            &self.transcript_path,
            &self.cwd,
            &format!("{:?}", self.permission_mode.load()),
            &self.current_model,
            &error_description,
        );

        self.fire_event(HookEvent::StopFailure, &input, None, None)
            .await;

        Ok(())
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
