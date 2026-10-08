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
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
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
    /// SessionStart `initialUserMessage` 的会话级准入闸门：每个会话至多投递一次。
    session_start_message_admitted: std::sync::atomic::AtomicBool,
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
            session_start_message_admitted: std::sync::atomic::AtomicBool::new(false),
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

    /// SessionStart `initialUserMessage`：受控准入一次，保留 hook 来源。
    ///
    /// 走 canonical reminder（source=hook）而不是伪造用户消息，因此不会被当成
    /// "用户亲自输入"；重复的 SessionStart 输出在会话内被抑制并可诊断。
    fn admit_session_start_message(&self, state: &dyn hook_state::HookOutputState, message: &str) {
        if self
            .session_start_message_admitted
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            tracing::debug!(
                field = "initialUserMessage",
                bytes = message.len(),
                "SessionStart initialUserMessage already admitted for this session"
            );
            return;
        }
        match hook_output_reminder(
            "session_start",
            "initial_user_message",
            message,
            &[ReminderAudience::Model],
        ) {
            Ok(reminder) => state.enqueue_session_start_message(reminder),
            Err(error) => tracing::warn!(
                field = "initialUserMessage",
                %error,
                "SessionStart initialUserMessage could not be delivered"
            ),
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
        self.fire_event_with_delivery(None, event, input, tool_name, tool_input)
            .await
    }

    /// 带投递面的 `fire_event`：M10 字段路由在唯一出口完成。
    ///
    /// 有投递面时按受众投递 `additionalContext` / `systemMessage`；没有投递面
    /// （例如直接调用 `fire_event` 的测试或只读阶段）时用可诊断状态记录，
    /// 不得静默丢弃，也不得"解析成功即假装生效"。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fire_event_with_delivery(
        &self,
        delivery: Option<&dyn hook_state::HookOutputState>,
        event: HookEvent,
        input: &HookInput,
        tool_name: Option<&str>,
        tool_input: Option<&serde_json::Value>,
    ) -> HookAction {
        let action = self
            .dispatcher
            .fire_event(event.clone(), input, tool_name, tool_input)
            .await;
        match delivery {
            Some(state) => route_hook_output(state, &event, &action),
            None => diagnose_undelivered_output(&event, &action),
        }
        action
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
                .fire_event_with_delivery(Some(state), HookEvent::SessionStart, &input, None, None)
                .await;
            action_resolver::resolve_action_to_result(
                &action,
                "SessionStart",
                "SessionStart hook prevented continuation",
            )?;
            if let HookAction::InitialUserMessage { message } = &action {
                self.admit_session_start_message(state, message);
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
            .fire_event_with_delivery(Some(state), HookEvent::UserPromptSubmit, &input, None, None)
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
        self.fire_event_with_delivery(
            Some(state),
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
            .fire_event_with_delivery(
                Some(state),
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
                ..
            } => {
                if let Some(new_input) = updated_input {
                    effective_call.input = new_input.clone();
                }
                // M10 字段路由：additionalContext / systemMessage 已由 fire_event
                // 出口统一诊断（不重复记录正文）。

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

            // P1-5: PermissionDenied —— 当 hook 拒绝权限时触发。
            // deny 也可能以 PermissionOverride（permissionDecision: deny/无法识别）
            // 归并而来，判定必须同时看两个形状。
            let override_denies = matches!(
                &action,
                HookAction::PermissionOverride { decision, .. } if decision.is_deny_like()
            );
            let is_denied = matches!(&action, HookAction::Block { .. })
                || matches!(&action, HookAction::PreventContinuation { .. })
                || override_denies;
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

            // PermissionOverride 的 deny/非法判定不经过 resolve_action_to_toolcall
            // （该归约只认 Block/PreventContinuation）：必须显式零执行，绝不放行。
            if override_denies {
                return Err(AgentError::ToolRejected {
                    tool: effective_call.name.clone(),
                    reason: HOOK_PERMISSION_DENY_REASON.to_string(),
                });
            }

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
        state: &mut dyn hook_state::AfterToolState,
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
            .fire_event_with_delivery(
                Some(state),
                event,
                &input,
                Some(&tool_call.name),
                Some(&tool_call.input),
            )
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

        let action = self
            .fire_event_with_delivery(Some(state), HookEvent::Stop, &input, None, None)
            .await;

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
        self.fire_event_with_delivery(Some(state), HookEvent::Notification, &input, None, None)
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

/// PermissionRequest `permissionDecision: deny` 的固定安全反馈：不透传 hook 自述文本。
const HOOK_PERMISSION_DENY_REASON: &str = "PermissionRequest hook denied this tool call";

/// PreToolUse `ask` 无审批端口时的固定反馈。
const HOOK_ASK_UNAVAILABLE_REASON: &str =
    "PreToolUse hook requested approval, but no approval channel is available";

/// PreToolUse `ask` 审批超时的固定反馈。
const HOOK_ASK_TIMEOUT_REASON: &str = "PreToolUse hook approval request timed out";

/// hook 输出正文的承载预算（UTF-8 字节）：超限按字符边界截断并显式标记，
/// 不静默裁掉内容。
const MAX_HOOK_OUTPUT_BYTES: usize = 32 * 1024;

/// 按字符边界把 hook 输出限制在承载预算内，并在截断时留下显式说明。
fn bounded_hook_output(text: &str) -> String {
    if text.len() <= MAX_HOOK_OUTPUT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_HOOK_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[hook output truncated: {} of {} bytes kept]",
        &text[..end],
        end,
        text.len()
    )
}

/// 事件名 → 稳定的 kind 片段（程序路由匹配 source + kind，不用展示文本）。
///
/// kind 是程序路由事实源，因此逐事件显式枚举：新增事件必须在此声明，未知事件
/// 也保留带原始事件名的可路由身份，不允许悄悄落进兜底分支。
fn event_kind(event: &HookEvent) -> String {
    match event {
        HookEvent::PreToolUse => "pre_tool_use",
        HookEvent::PostToolUse => "post_tool_use",
        HookEvent::PostToolUseFailure => "post_tool_use_failure",
        HookEvent::PostToolBatch => "post_tool_batch",
        HookEvent::PermissionRequest => "permission_request",
        HookEvent::PermissionDenied => "permission_denied",
        HookEvent::UserPromptSubmit => "user_prompt_submit",
        HookEvent::SessionStart => "session_start",
        HookEvent::SessionEnd => "session_end",
        HookEvent::Stop => "stop",
        HookEvent::StopFailure => "stop_failure",
        HookEvent::SubagentStart => "subagent_start",
        HookEvent::SubagentStop => "subagent_stop",
        HookEvent::PreCompact => "pre_compact",
        HookEvent::PostCompact => "post_compact",
        HookEvent::Notification => "notification",
        HookEvent::Setup => "setup",
        HookEvent::TeammateIdle => "teammate_idle",
        HookEvent::TaskCreated => "task_created",
        HookEvent::TaskCompleted => "task_completed",
        HookEvent::ConfigChange => "config_change",
        HookEvent::WorktreeCreate => "worktree_create",
        HookEvent::WorktreeRemove => "worktree_remove",
        HookEvent::InstructionsLoaded => "instructions_loaded",
        HookEvent::Elicitation => "elicitation",
        HookEvent::ElicitationResult => "elicitation_result",
        HookEvent::CwdChanged => "cwd_changed",
        HookEvent::FileChanged => "file_changed",
        HookEvent::Unknown(name) => return format!("unknown:{name}"),
    }
    .to_string()
}

/// M10 字段路由：把 hook 输出投递到其声明受众（单一实现，供各阶段共用）。
///
/// - `additionalContext` → 有界、带 hook 来源的 Model reminder（不唤醒新一轮）；
/// - `systemMessage` → 客户端提示（Tui 受众，不进模型上下文）；
/// - 无法在可承载预算内构造的字段：显式诊断，不静默丢弃、不假装生效。
fn route_hook_output(
    state: &dyn hook_state::HookOutputState,
    event: &HookEvent,
    action: &HookAction,
) {
    let (additional_context, system_message) = match action {
        HookAction::AdditionalContext { context } => (Some(context.as_str()), None),
        HookAction::SystemMessage { message } => (None, Some(message.as_str())),
        HookAction::PermissionOverride {
            additional_context,
            system_message,
            ..
        } => (additional_context.as_deref(), system_message.as_deref()),
        _ => (None, None),
    };
    let kind = event_kind(event);

    if let Some(context) = additional_context {
        match hook_output_reminder(
            &kind,
            "additional_context",
            context,
            &[ReminderAudience::Model],
        ) {
            Ok(reminder) => state.enqueue_hook_model_reminder(reminder),
            Err(error) => tracing::warn!(
                event = ?event,
                field = "additionalContext",
                %error,
                "hook additionalContext could not be delivered as a model reminder"
            ),
        }
    }

    if let Some(message) = system_message {
        match hook_output_reminder(&kind, "system_message", message, &[ReminderAudience::Tui]) {
            Ok(reminder) => state.enqueue_hook_client_notice(reminder),
            Err(error) => tracing::warn!(
                event = ?event,
                field = "systemMessage",
                %error,
                "hook systemMessage could not be delivered as a client notice"
            ),
        }
    }
}

/// 构造一条 hook 输出 reminder：正文有界、来源为 hook、受众显式声明。
fn hook_output_reminder(
    kind: &str,
    field: &str,
    text: &str,
    audiences: &[ReminderAudience],
) -> Result<TrustedSystemReminder, AgentError> {
    let body = bounded_hook_output(text);
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Guidance,
            source: ReminderSource("hook".into()),
            kind: format!("{kind}_{field}"),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(audiences.to_vec()),
            body,
            summary: Some(format!("hook {kind} {field}")),
            metadata: json!({
                "event": kind,
                "field": field,
                "bytes": text.len(),
            }),
        })
        .map_err(|error| AgentError::MiddlewareError {
            middleware: "HookMiddleware".to_string(),
            reason: format!("hook output reminder rejected: {error}"),
        })
}

/// 已解析但投递尚未接线的 hook 输出：只记录事件、字段名与长度，绝不记录正文。
///
/// 仅在**没有投递面**的调用点使用（直接调用 `fire_event` 的用法、或只读阶段
/// 如 StopFailure）。生产阶段方法必须经
/// [`HookMiddleware::fire_event_with_delivery`] 走真实投递；未接线期间不得静默
/// 丢弃，也不得"解析成功即假装生效"。
fn diagnose_undelivered_output(event: &HookEvent, action: &HookAction) {
    let (additional_context, system_message) = match action {
        HookAction::AdditionalContext { context } => (Some(context.as_str()), None),
        HookAction::SystemMessage { message } => (None, Some(message.as_str())),
        HookAction::PermissionOverride {
            additional_context,
            system_message,
            ..
        } => (additional_context.as_deref(), system_message.as_deref()),
        _ => (None, None),
    };
    if let Some(context) = additional_context {
        tracing::debug!(
            event = ?event,
            field = "additionalContext",
            bytes = context.len(),
            "hook output parsed but delivery is not wired yet (M10 → F)"
        );
    }
    if let Some(message) = system_message {
        tracing::debug!(
            event = ?event,
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

#[cfg(test)]
#[path = "undelivered_output_test.rs"]
mod undelivered_output_tests;

#[cfg(test)]
#[path = "hook_output_delivery_test.rs"]
mod hook_output_delivery_tests;

#[cfg(test)]
#[path = "permission_request_deny_test.rs"]
mod permission_request_deny_tests;
