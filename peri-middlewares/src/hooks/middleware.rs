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
use peri_agent::middleware::capabilities as hook_state;
// 函数已迁移到 `dispatcher.rs`，此处保留 pub use 以维持 ABI。
pub use crate::hooks::dispatcher::fire_standalone_lifecycle_hooks;

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use peri_agent::{
    agent::react::{AgentOutput, ReactLLM, ToolCall, ToolResult},
    error::{AgentError, AgentResult},
    messages::BaseMessage,
    middleware::r#trait::Middleware,
    session::{MessageKind, MessageSource, QueuedMessage},
};
use serde_json::json;

use crate::permission::SharedPermissionMode;
use peri_agent::interaction::{
    ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse, UserInteractionBroker,
};
// HookType 仅 `middleware_test.rs` 通过 `use super::*` 使用。保留以维持测试不变。
#[allow(unused_imports)]
use crate::hooks::{
    action_resolver,
    dispatcher::HookDispatcher,
    input_builder,
    once_tracker::OnceTracker,
    permission_gate,
    stop_block_guard::{
        format_post_tool_batch_feedback_no_wrapper, format_post_tool_batch_stop_intent,
        format_stop_block_feedback_no_wrapper, GuardDecision, StopBlockGuard,
    },
    types::{HookAction, HookEvent, HookInput, HookType, PermissionDecision, RegisteredHook},
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
    /// 默认使用 [`crate::permission::default_requires_approval`]，
    /// 可通过 `with_requires_approval` 覆盖。
    requires_approval: fn(&str) -> bool,
    /// Stop hook block 连续次数计数器（最多 8 次，超过后忽略）
    stop_block_guard: Arc<StopBlockGuard>,
    /// PreToolUse `ask` 的审批端口（与 PermissionMiddleware 同源 broker）。
    /// None = 无审批通道：宿主也不会弹窗时，ask 必须拒绝而不是放行。
    broker: Option<Arc<dyn UserInteractionBroker>>,
    /// 宿主审批路径（PermissionMiddleware）是否真的在链上。
    ///
    /// `should_fire_permission_request_*` 只描述"权限面存在时会不会弹窗"；
    /// MetaHarness 关闭 Permission 面后 Default 模式同样不会有人弹审批。
    /// 缺失（默认）时 ask 不允许交宿主，必须走 `broker`；无 broker 明确拒绝。
    host_approval_path: bool,
    /// broker.request 超时（与 PermissionMiddleware 同源常量）
    broker_timeout: std::time::Duration,
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
            requires_approval: crate::permission::default_requires_approval,
            stop_block_guard,
            broker: None,
            // fail-closed 默认：未显式声明宿主审批面在链上时，ask 不得交宿主。
            host_approval_path: false,
            broker_timeout: crate::permission::BROKER_TIMEOUT,
        }
    }

    /// 声明宿主审批路径（PermissionMiddleware）是否在链上。
    ///
    /// 由装配点按 MetaHarness 关闭集注入（关闭 Permission 面 → false）；
    /// 缺失即 false：ask 走 broker，无 broker 明确拒绝。
    pub fn with_host_approval_path(mut self, present: bool) -> Self {
        self.host_approval_path = present;
        self
    }

    /// 注入审批端口（PreToolUse `ask` 落实用）。
    ///
    /// 与 `PermissionMiddleware` 使用同一 broker 源；未注入时，宿主权限路径不会
    /// 弹窗的场景下 ask 必须拒绝（见 [`Self::resolve_ask_approval`]）。
    pub fn with_broker(mut self, broker: Arc<dyn UserInteractionBroker>) -> Self {
        self.broker = Some(broker);
        self
    }

    /// 设置 ask 审批超时（测试用）
    pub fn with_broker_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.broker_timeout = timeout;
        self
    }

    /// 落实 PreToolUse `ask`。
    ///
    /// - 宿主权限路径确实在链上，且本来就会弹窗（`should_fire_permission_request_*`）
    ///   → 返回 `None` 交给宿主审批，避免同一次调用出现双重审批；
    /// - 宿主路径缺失（MetaHarness 关闭 Permission 面）或本就不会弹窗（Bypass、
    ///   豁免工具等）→ 必须经既有 `UserInteractionBroker` 有界审批；无 broker、
    ///   超时、拒绝都返回固定反馈的 `ToolRejected`，绝不放行。
    async fn resolve_ask_approval(
        &self,
        state: &mut dyn hook_state::BeforeToolState,
        tool_call: &ToolCall,
    ) -> AgentResult<Option<ToolCall>> {
        let origin = state.tool_origin(&tool_call.id);
        let host_will_ask = self.host_approval_path
            && permission_gate::should_fire_permission_request_for_origin(
                self.permission_mode.load(),
                &tool_call.name,
                self.requires_approval,
                origin.as_ref(),
            );
        if host_will_ask {
            return Ok(None);
        }

        let Some(broker) = &self.broker else {
            return Err(AgentError::ToolRejected {
                tool: tool_call.name.clone(),
                reason: HOOK_ASK_UNAVAILABLE_REASON.to_string(),
            });
        };

        let ctx = InteractionContext::Approval {
            items: vec![ApprovalItem {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                tool_input: tool_call.input.clone(),
            }],
        };
        let response = match peri_time::timeout(self.broker_timeout, broker.request(ctx)).await {
            Ok(response) => response,
            Err(_) => {
                return Err(AgentError::ToolRejected {
                    tool: tool_call.name.clone(),
                    reason: HOOK_ASK_TIMEOUT_REASON.to_string(),
                });
            }
        };
        let decision = match response {
            InteractionResponse::Decisions(mut decisions) => {
                decisions.pop().unwrap_or_else(|| ApprovalDecision::Reject {
                    reason: "用户拒绝".to_string(),
                    source: None,
                })
            }
            _ => ApprovalDecision::Reject {
                reason: "用户拒绝".to_string(),
                source: None,
            },
        };
        crate::permission::apply_decision(tool_call, decision).map(Some)
    }

    /// Keep asynchronous hooks and command processes in the session execution scope.
    pub fn with_task_manager(
        mut self,
        task_manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    ) -> Self {
        self.dispatcher = self.dispatcher.with_task_manager(task_manager);
        self
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
    ///
    /// 工具结果已提交，因此这里不存在「拒绝」语义：
    /// - Block ⇒ 回注有界反馈（复用 stop_block_guard 防循环计数，模型下一请求可见）；
    /// - `continue:false` ⇒ 投递显式停止意图，经 Receive 唯一退出口停止，不发起额外请求。
    pub async fn fire_post_tool_batch(
        &self,
        state: &mut dyn hook_state::AfterToolsBatchState,
    ) -> AgentResult<()> {
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

        match action_resolver::resolve_post_tool_batch_action(&action) {
            action_resolver::PostToolBatchDecision::Continue => {
                self.stop_block_guard.on_non_block();
                Ok(())
            }
            action_resolver::PostToolBatchDecision::Feedback { reason } => {
                match self.stop_block_guard.on_block(&reason) {
                    // 防循环上限：忽略 block，正常继续（不再回注）
                    GuardDecision::ForceFinish | GuardDecision::Pass => Ok(()),
                    GuardDecision::Block { count, reason } => {
                        let feedback = format_post_tool_batch_feedback_no_wrapper(&reason, count);
                        let reminder = TrustedSystemReminderFactory::for_producer()
                            .construct(SystemReminder {
                                version: SYSTEM_REMINDER_VERSION,
                                category: ReminderCategory::Guidance,
                                source: ReminderSource("hook".into()),
                                kind: "post_tool_batch_blocked".into(),
                                severity: ReminderSeverity::Warning,
                                delivery: ReminderDelivery::Required,
                                audiences: ReminderAudiences(vec![
                                    ReminderAudience::Model,
                                    ReminderAudience::Automation,
                                ]),
                                body: feedback,
                                summary: Some(format!("PostToolBatch hook 阻止继续（{count}/8）")),
                                metadata: json!({ "block_count": count }),
                            })
                            .map_err(|error| AgentError::MiddlewareError {
                                middleware: self.name().to_string(),
                                reason: error.to_string(),
                            })?;
                        state.enqueue_batch_feedback(reminder);
                        Ok(())
                    }
                }
            }
            action_resolver::PostToolBatchDecision::Stop { stop_reason } => {
                self.stop_block_guard.on_non_block();
                let body = format_post_tool_batch_stop_intent(stop_reason.as_deref());
                let reminder = TrustedSystemReminderFactory::for_producer()
                    .construct(SystemReminder {
                        version: SYSTEM_REMINDER_VERSION,
                        category: ReminderCategory::Lifecycle,
                        source: ReminderSource("hook".into()),
                        kind: "post_tool_batch_stop".into(),
                        severity: ReminderSeverity::Info,
                        delivery: ReminderDelivery::Configurable,
                        audiences: ReminderAudiences(vec![
                            ReminderAudience::Tui,
                            ReminderAudience::Automation,
                        ]),
                        body,
                        summary: Some("PostToolBatch hook 请求停止本轮".into()),
                        metadata: json!({}),
                    })
                    .map_err(|error| AgentError::MiddlewareError {
                        middleware: self.name().to_string(),
                        reason: error.to_string(),
                    })?;
                state.enqueue_stop_intent(reminder);
                Ok(())
            }
        }
    }
}

#[async_trait]
impl Middleware for HookMiddleware {
    fn name(&self) -> &str {
        "HookMiddleware"
    }

    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
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
                    diagnose_undelivered_output(None, Some(message), "SessionStart");
                }
                HookAction::AdditionalContext { context } => {
                    diagnose_undelivered_output(Some(context), None, "SessionStart");
                }
                HookAction::InitialUserMessage { message } => {
                    tracing::debug!(
                        event = "SessionStart",
                        field = "initialUserMessage",
                        bytes = message.len(),
                        "hook output parsed but delivery is not wired yet (M10 → F)"
                    );
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

        // P1-5: InstructionsLoaded —— 每次 user prompt 时规则/skill 已加载
        let instructions_input = HookInput {
            session_id: self.session_id.clone(),
            transcript_path: self.transcript_path.clone(),
            cwd: self.cwd.clone(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::InstructionsLoaded,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: Some(prompt),
            source: None,
            model: Some(self.current_model.clone()),
            subagent_name: None,
            subagent_result: None,
            message_count: None,
            additional_data: None,
        };
        self.fire_event(
            HookEvent::InstructionsLoaded,
            &instructions_input,
            None,
            None,
        )
        .await;

        Ok(())
    }

    async fn before_tool(
        &self,
        state: &mut dyn hook_state::BeforeToolState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        let permission_mode_str = format!("{:?}", self.permission_mode.load());
        let mut input = HookInput::tool_call(
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
        // PreToolUse 归并结果：deny 零执行 > updatedInput > allow/passthrough 交宿主。
        let mut effective_call = tool_call.clone();
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
            HookAction::PermissionOverride {
                decision,
                updated_input,
                additional_context,
                system_message,
                ..
            } => {
                if let Some(new_input) = updated_input {
                    effective_call.input = new_input.clone();
                }
                // M10 字段路由：已解析但投递尚未接线的字段必须可诊断，不记录正文
                diagnose_undelivered_output(
                    additional_context.as_deref(),
                    system_message.as_deref(),
                    "PreToolUse",
                );

                // deny（含无法识别的判定）优先于 updatedInput：零工具执行 + 固定安全反馈
                if decision.is_deny_like() {
                    return Err(AgentError::ToolRejected {
                        tool: effective_call.name.clone(),
                        reason: HOOK_DENY_REASON.to_string(),
                    });
                }

                if matches!(decision, PermissionDecision::Ask) {
                    if let Some(approved) =
                        self.resolve_ask_approval(state, &effective_call).await?
                    {
                        effective_call = approved;
                    }
                }
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
        if effective_call.input != tool_call.input {
            input.tool_input = Some(effective_call.input.clone());
        }
        let origin = state.tool_origin(&effective_call.id);
        let should_fire = permission_gate::should_fire_permission_request_for_origin(
            self.permission_mode.load(),
            &effective_call.name,
            self.requires_approval,
            origin.as_ref(),
        );

        if should_fire {
            let action = self
                .fire_event(
                    HookEvent::PermissionRequest,
                    &input,
                    Some(&effective_call.name),
                    Some(&effective_call.input),
                )
                .await;

            // P1-5: PermissionDenied —— 当 hook 拒绝权限时触发
            let is_denied = matches!(&action, HookAction::Block { .. })
                || matches!(&action, HookAction::PreventContinuation { .. });
            if is_denied {
                self.fire_event(
                    HookEvent::PermissionDenied,
                    &input,
                    Some(&effective_call.name),
                    Some(&effective_call.input),
                )
                .await;
            }

            // Fire Notification (agent is waiting for user permission)
            self.fire_event(
                HookEvent::Notification,
                &input,
                Some(&effective_call.name),
                Some(&effective_call.input),
            )
            .await;

            return action_resolver::resolve_action_to_toolcall(
                &action,
                &effective_call,
                "Hook prevented continuation",
            );
        }

        Ok(effective_call)
    }

    async fn after_tool(
        &self,
        _state: &mut dyn hook_state::AfterToolState,
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
        state: &mut dyn hook_state::AfterToolsBatchState,
        _results: &[(ToolCall, ToolResult)],
    ) -> AgentResult<()> {
        self.fire_post_tool_batch(state).await
    }

    async fn after_agent(
        &self,
        state: &mut dyn hook_state::AfterAgentState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
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
                    let execution =
                        state
                            .execution_binding()
                            .ok_or_else(|| AgentError::MiddlewareError {
                                middleware: self.name().to_string(),
                                reason: "Stop hook steering requires a current execution binding"
                                    .into(),
                            })?;
                    let feedback = format_stop_block_feedback_no_wrapper(&reason, count);
                    let reminder = TrustedSystemReminderFactory::for_producer()
                        .construct(SystemReminder {
                            version: SYSTEM_REMINDER_VERSION,
                            category: ReminderCategory::Guidance,
                            source: ReminderSource("hook".into()),
                            kind: "stop_blocked".into(),
                            severity: ReminderSeverity::Warning,
                            delivery: ReminderDelivery::Required,
                            audiences: ReminderAudiences(vec![
                                ReminderAudience::Model,
                                ReminderAudience::Automation,
                            ]),
                            body: feedback,
                            summary: Some(format!("Stop hook 阻止结束（{count}/8）")),
                            metadata: json!({ "block_count": count }),
                        })
                        .map_err(|error| AgentError::MiddlewareError {
                            middleware: self.name().to_string(),
                            reason: error.to_string(),
                        })?;
                    state.enqueue_v2_message(
                        QueuedMessage::system_reminder(
                            MessageKind::Defer,
                            MessageSource::StopHookFeedback,
                            reminder,
                        )
                        .with_policy(
                            peri_acp_types::session::MessagePolicy::continue_current_run(execution),
                        ),
                    );
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

    async fn on_error(
        &self,
        _state: &mut dyn hook_state::StateView,
        error: &AgentError,
    ) -> AgentResult<()> {
        // StopFailure 仅在 API/LLM 调用失败时触发，
        // 跳过 Interrupted、MaxIterationsExceeded、ToolRejected 等非 API 错误。
        let should_fire = matches!(
            error,
            AgentError::LlmError(_)
                | AgentError::LlmHttpError { .. }
                | AgentError::ModelError(..)
                | AgentError::StreamRecoveryExhausted { .. }
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

/// PreToolUse `deny` 的固定安全反馈：不透传 hook 自述文本。
const HOOK_DENY_REASON: &str = "PreToolUse hook denied this tool call";

/// PreToolUse `ask` 无审批端口时的固定反馈。
const HOOK_ASK_UNAVAILABLE_REASON: &str =
    "PreToolUse hook requested approval, but no approval channel is available";

/// PreToolUse `ask` 审批超时的固定反馈。
const HOOK_ASK_TIMEOUT_REASON: &str = "PreToolUse hook approval request timed out";

/// 已解析但投递尚未接线的 hook 输出：只记录字段名与长度，绝不记录正文。
///
/// M10 字段路由（additionalContext / systemMessage）在 C 组只完成类型与可诊断
/// 状态，实际投递由 F 组接线；未接线期间不得静默丢弃，也不得“解析成功即假装生效”。
fn diagnose_undelivered_output(
    additional_context: Option<&str>,
    system_message: Option<&str>,
    event: &str,
) {
    if let Some(context) = additional_context {
        tracing::debug!(
            event,
            field = "additionalContext",
            bytes = context.len(),
            "hook output parsed but delivery is not wired yet (M10 → F)"
        );
    }
    if let Some(message) = system_message {
        tracing::debug!(
            event,
            field = "systemMessage",
            bytes = message.len(),
            "hook output parsed but delivery is not wired yet (M10 → F)"
        );
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;

#[cfg(test)]
#[path = "post_tool_batch_test.rs"]
mod post_tool_batch_tests;

#[cfg(test)]
#[path = "async_diagnostic_test.rs"]
mod async_diagnostic_tests;

#[cfg(test)]
#[path = "ask_host_path_test.rs"]
mod ask_host_path_tests;
