use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use peri_agent::{
    agent::{
        react::{AgentOutput, ReactLLM, ToolCall, ToolResult},
        state::State,
    },
    error::{AgentError, AgentResult},
    messages::BaseMessage,
    middleware::Middleware,
};

use crate::hitl::{PermissionMode, SharedPermissionMode};
use crate::hooks::{
    executor::{execute_agent_hook, execute_command_hook, execute_http_hook, execute_prompt_hook},
    matcher::{matches_if_condition, matches_matcher},
    types::{HookAction, HookEvent, HookInput, HookType, RegisteredHook},
};

/// Plugin hook middleware — fires registered hooks at lifecycle events.
pub struct HookMiddleware {
    hooks: Arc<RwLock<HashMap<HookEvent, Vec<RegisteredHook>>>>,
    llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
    cwd: String,
    session_id: String,
    transcript_path: String,
    /// 共享权限模式（运行时可变，Shift+Tab 切换）。
    /// PermissionRequest 仅在权限对话框即将展示时触发。
    permission_mode: Arc<SharedPermissionMode>,
    current_model: String,
    once_fired: Arc<Mutex<HashSet<String>>>,
    /// SessionStart 的 source 值（"startup"/"resume"/"clear"/"compact"）。
    /// None 表示不触发 SessionStart。
    session_start_source: Option<String>,
    /// 判断工具是否需要用户审批。用于 PermissionRequest hook 门控。
    /// 默认使用 [`crate::hitl::default_requires_approval`]，
    /// 可通过 `with_requires_approval` 覆盖。
    requires_approval: fn(&str) -> bool,
    /// Stop hook block 连续次数计数器（最多 8 次，超过后忽略）
    stop_block_count: Arc<Mutex<u32>>,
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
        Self {
            hooks: Arc::new(RwLock::new(map)),
            llm_factory,
            cwd: cwd.into(),
            session_id: session_id.into(),
            transcript_path: transcript_path.into(),
            permission_mode,
            current_model: current_model.into(),
            once_fired: Arc::new(Mutex::new(HashSet::new())),
            session_start_source,
            requires_approval: crate::hitl::default_requires_approval,
            stop_block_count: Arc::new(Mutex::new(0)),
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// 判断当前权限模式下，给定工具是否会触发权限对话框。
    ///
    /// 对齐 Claude Code：PermissionRequest 仅在权限对话框即将展示时触发。
    fn needs_permission_dialog(&self, tool_name: &str) -> bool {
        match self.permission_mode.load() {
            // Bypass: 所有工具直接放行，无对话框
            PermissionMode::Bypass => false,
            // DontAsk: 直接拒绝敏感工具，无对话框
            PermissionMode::DontAsk => false,
            // AcceptEdit: 编辑工具放行，其他弹窗
            PermissionMode::AcceptEdit => !crate::hitl::is_edit_tool(tool_name),
            // AutoMode: 分类器决定；简化处理——当无分类器或 Unsure 时弹窗
            // 为避免 hook 系统依赖分类器，AutoMode 下始终触发 PermissionRequest
            PermissionMode::AutoMode => true,
            // Default: 敏感工具始终弹窗
            PermissionMode::Default => true,
        }
    }

    // -----------------------------------------------------------------------
    // fire_event — core dispatch loop
    // -----------------------------------------------------------------------

    async fn fire_event(
        &self,
        event: HookEvent,
        input: &HookInput,
        tool_name: Option<&str>,
        tool_input: Option<&serde_json::Value>,
    ) -> HookAction {
        // 确保 hook_event_name 与实际触发的事件一致。
        //
        // 调用方可能在 before_tool 中复用同一个 HookInput 连续触发多个事件
        // （PreToolUse → PermissionRequest → Notification），而 HookInput::tool_call()
        // 构造函数硬编码 hook_event_name = PreToolUse。若不修正，PermissionRequest hook
        // 脚本从 stdin 读到的 hook_event_name 会是 "PreToolUse" 而非 "PermissionRequest"。
        let input = if input.hook_event_name != event {
            let mut corrected = input.clone();
            corrected.hook_event_name = event.clone();
            corrected
        } else {
            input.clone()
        };

        let hooks = {
            let map = self.hooks.read();
            match map.get(&event) {
                Some(h) => {
                    tracing::debug!(
                        event = ?event,
                        count = h.len(),
                        "HookMiddleware: found hooks for event"
                    );
                    h.clone()
                }
                None => {
                    return HookAction::Allow;
                }
            }
        };

        if hooks.is_empty() {
            return HookAction::Allow;
        }

        let mut final_action = HookAction::Allow;

        for registered in &hooks {
            // once check
            if Self::is_once_hook(&registered.hook) && self.was_once_fired(registered) {
                continue;
            }

            // matcher check
            if let Some(name) = tool_name {
                let matcher_str = registered.matcher.as_deref().unwrap_or_else(|| {
                    registered
                        .hook
                        .get_matcher()
                        .map(|s| s.as_str())
                        .unwrap_or("*")
                });
                if !matches_matcher(matcher_str, name) {
                    continue;
                }
            }

            // if condition check
            if let Some(condition) = registered.hook.get_condition() {
                if let (Some(name), Some(inp)) = (tool_name, tool_input) {
                    if !matches_if_condition(condition, name, inp) {
                        continue;
                    }
                }
            }

            // Execute hook (async hooks are spawned in background, result ignored)
            if let Some(ref msg) = registered.hook.get_status_message() {
                tracing::info!(
                    plugin = %registered.plugin_name,
                    event = ?event,
                    "Hook status: {}",
                    msg
                );
            }
            let action = if registered.hook.is_async() {
                // Fire-and-forget: spawn in background, return Allow immediately
                let hook = registered.hook.clone();
                let owned_input = input.clone();
                let registered = registered.clone();
                tokio::spawn(async move {
                    let _ = match &hook {
                        HookType::Command { .. } => {
                            execute_command_hook(&hook, &owned_input, &registered).await
                        }
                        HookType::Http { .. } => execute_http_hook(&hook, &owned_input).await,
                        // Prompt/Agent hooks need LLM factory which can't be cloned into spawn;
                        // async only applies to Command per schema definition.
                        _ => HookAction::Allow,
                    };
                });
                HookAction::Allow
            } else {
                match &registered.hook {
                    HookType::Command { .. } => {
                        execute_command_hook(&registered.hook, &input, registered).await
                    }
                    HookType::Prompt { .. } => {
                        execute_prompt_hook(&registered.hook, &input, &self.llm_factory).await
                    }
                    HookType::Http { .. } => execute_http_hook(&registered.hook, &input).await,
                    HookType::Agent { .. } => {
                        execute_agent_hook(&registered.hook, &input, &self.llm_factory, &self.cwd)
                            .await
                    }
                }
            };

            // once mark
            if Self::is_once_hook(&registered.hook) {
                self.mark_once_fired(registered);
            }

            // Short-circuit on Block / PreventContinuation
            match &action {
                HookAction::Block { .. } | HookAction::PreventContinuation { .. } => return action,
                HookAction::ModifyInput { new_input } => {
                    final_action = HookAction::ModifyInput {
                        new_input: new_input.clone(),
                    };
                }
                HookAction::PermissionOverride { decision, reason } => {
                    // Phase 2: 权限覆盖决策暂不改变实际权限行为，仅记录
                    tracing::debug!(
                        "PermissionOverride from hook: {:?} (reason: {:?})",
                        decision,
                        reason
                    );
                    final_action = HookAction::PermissionOverride {
                        decision: decision.clone(),
                        reason: reason.clone(),
                    };
                }
                _ => {}
            }
        }

        final_action
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

        let input = HookInput {
            session_id: self.session_id.clone(),
            transcript_path: self.transcript_path.clone(),
            cwd: self.cwd.clone(),
            permission_mode: Some(format!("{:?}", self.permission_mode.load())),
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::PostToolBatch,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: Some(prompt_text),
            source: None,
            model: Some(self.current_model.clone()),
            subagent_name: None,
            subagent_result: None,
            message_count: Some(state.messages().len()),
        };

        let action = self
            .fire_event(HookEvent::PostToolBatch, &input, None, None)
            .await;

        match &action {
            HookAction::Block { reason } => Err(AgentError::ToolRejected {
                tool: "PostToolBatch".to_string(),
                reason: reason.clone(),
            }),
            HookAction::PreventContinuation { stop_reason } => Err(AgentError::ToolRejected {
                tool: "PostToolBatch".to_string(),
                reason: stop_reason
                    .clone()
                    .unwrap_or_else(|| "PostToolBatch hook prevented continuation".to_string()),
            }),
            _ => Ok(()),
        }
    }

    // -----------------------------------------------------------------------
    // Helper methods
    // -----------------------------------------------------------------------

    fn is_once_hook(hook: &HookType) -> bool {
        hook.is_once()
    }

    fn once_key(registered: &RegisteredHook) -> String {
        format!(
            "{}:{}:{:?}",
            registered.plugin_id,
            serde_json::to_string(&registered.hook).unwrap_or_default(),
            registered.event
        )
    }

    fn was_once_fired(&self, registered: &RegisteredHook) -> bool {
        let key = Self::once_key(registered);
        self.once_fired.lock().contains(&key)
    }

    fn mark_once_fired(&self, registered: &RegisteredHook) {
        let key = Self::once_key(registered);
        self.once_fired.lock().insert(key);
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
            match &action {
                HookAction::Block { reason } => {
                    return Err(AgentError::ToolRejected {
                        tool: "SessionStart".to_string(),
                        reason: reason.clone(),
                    });
                }
                HookAction::PreventContinuation { stop_reason } => {
                    let reason = stop_reason
                        .clone()
                        .unwrap_or_else(|| "SessionStart hook prevented continuation".to_string());
                    return Err(AgentError::ToolRejected {
                        tool: "SessionStart".to_string(),
                        reason,
                    });
                }
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

        // Handle UserPromptSubmit actions
        match &action {
            HookAction::Block { reason } => {
                return Err(AgentError::ToolRejected {
                    tool: "UserPromptSubmit".to_string(),
                    reason: reason.clone(),
                });
            }
            HookAction::PreventContinuation { stop_reason } => {
                let reason = stop_reason
                    .clone()
                    .unwrap_or_else(|| "Hook prevented continuation".to_string());
                return Err(AgentError::ToolRejected {
                    tool: "UserPromptSubmit".to_string(),
                    reason,
                });
            }
            _ => {}
        }

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

        match &action {
            HookAction::Block { reason } => {
                return Err(AgentError::ToolRejected {
                    tool: tool_call.name.clone(),
                    reason: reason.clone(),
                });
            }
            HookAction::PreventContinuation { stop_reason } => {
                let reason = stop_reason
                    .clone()
                    .unwrap_or_else(|| "Hook prevented continuation".to_string());
                return Err(AgentError::ToolRejected {
                    tool: tool_call.name.clone(),
                    reason,
                });
            }
            HookAction::ModifyInput { new_input } => {
                return Ok(ToolCall {
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    input: new_input.clone(),
                });
            }
            _ => {}
        }

        // PermissionRequest 门控：仅对敏感工具 + 权限对话框即将展示时触发。
        //
        // Claude Code 行为：PermissionRequest 仅在权限对话框即将展示给用户时触发。
        // Bypass/AutoMode(DontAsk 的 auto-allow 路径) 不展示对话框，因此不触发。
        //
        // 使用 hitl::default_requires_approval 判断工具是否需要审批（Bash/Write/Edit/Agent/
        // mcp__*/WebFetch/WebSearch 等）。非敏感工具（Read/Glob/Grep 等）不触发。
        let is_sensitive = (self.requires_approval)(&tool_call.name);
        let needs_dialog = self.needs_permission_dialog(&tool_call.name);

        if is_sensitive && needs_dialog {
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

            match &action {
                HookAction::Block { reason } => {
                    return Err(AgentError::ToolRejected {
                        tool: tool_call.name.clone(),
                        reason: reason.clone(),
                    });
                }
                HookAction::PreventContinuation { stop_reason } => {
                    let reason = stop_reason
                        .clone()
                        .unwrap_or_else(|| "Hook prevented continuation".to_string());
                    return Err(AgentError::ToolRejected {
                        tool: tool_call.name.clone(),
                        reason,
                    });
                }
                HookAction::ModifyInput { new_input } => {
                    return Ok(ToolCall {
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        input: new_input.clone(),
                    });
                }
                _ => {}
            }
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
            &serde_json::json!(result.output),
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
        // 构造 Stop hook 的 HookInput。
        // subagent_result 携带 agent 最终输出（截断到 500 字符），
        // source 携带 stop_reason（若存在）标识结束原因。
        let input = HookInput {
            session_id: self.session_id.clone(),
            transcript_path: self.transcript_path.clone(),
            cwd: self.cwd.clone(),
            permission_mode: Some(format!("{:?}", self.permission_mode.load())),
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::Stop,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: output
                .stop_reason
                .as_deref()
                .map(|_| "agent_complete".to_string()),
            model: Some(self.current_model.clone()),
            subagent_name: None,
            subagent_result: Some(output.text.chars().take(500).collect::<String>()),
            message_count: None,
        };

        let action = self.fire_event(HookEvent::Stop, &input, None, None).await;

        match &action {
            HookAction::Block { reason } => {
                let mut count = self.stop_block_count.lock();
                *count += 1;
                if *count > 8 {
                    tracing::warn!(
                        count = *count,
                        "Stop hook block 连续超过 8 次，忽略 block 正常结束"
                    );
                    // Reset counter and let agent finish normally
                    return Ok(output.clone());
                }
                tracing::info!(
                    count = *count,
                    reason = %reason,
                    "Stop hook blocked: injecting reason as system-reminder (Human) and continuing"
                );
                // [TRAP] 必须用 Human + <system-reminder> 注入，禁止 BaseMessage::system。
                // System 消息会被 anthropic/openai invoke hoist 到 system prompt 顶部，
                // 违反 frozen_system_prompt 稳定性（第一优先级）。
                // （与 goal_middleware.rs / compact_middleware.rs 注入路径一致）
                state.add_message(BaseMessage::human(format!(
                    "<system-reminder>\n<stop_hook_feedback>\nThe Stop hook blocked because: {}\nPlease address this feedback and continue your work.\n(Block {}/8)\n</stop_hook_feedback>\n</system-reminder>",
                    reason, *count
                )));
                let mut output = output.clone();
                output.block_continue = Some(reason.clone());
                return Ok(output);
            }
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
                *self.stop_block_count.lock() = 0;
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
        let input = HookInput {
            session_id: self.session_id.clone(),
            transcript_path: self.transcript_path.clone(),
            cwd: self.cwd.clone(),
            permission_mode: Some(format!("{:?}", self.permission_mode.load())),
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::StopFailure,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: Some(serde_json::json!(error_description)),
            prompt: None,
            source: None,
            model: Some(self.current_model.clone()),
            subagent_name: None,
            subagent_result: None,
            message_count: None,
        };

        self.fire_event(HookEvent::StopFailure, &input, None, None)
            .await;

        Ok(())
    }
}

/// Fire standalone lifecycle hooks outside of the middleware lifecycle.
///
/// Used by the TUI layer for events that occur outside the agent ReAct loop:
/// - `SessionEnd`: when `/clear` resets the session
/// - `PreCompact` / `PostCompact`: before/after context compaction
/// - `Notification`: when agent needs user attention (e.g. AskUserQuestion)
///
/// The HookMiddleware instance is owned by the agent task and not accessible
/// from these code paths, so we dispatch hooks directly.
#[allow(clippy::too_many_arguments)]
pub async fn fire_standalone_lifecycle_hooks(
    registered_hooks: &[RegisteredHook],
    event: HookEvent,
    cwd: &str,
    session_id: &str,
    transcript_path: &str,
    current_model: &str,
    message_count: Option<usize>,
    reason: Option<&str>,
) {
    // Filter hooks matching the event
    let matching: Vec<&RegisteredHook> = registered_hooks
        .iter()
        .filter(|h| h.event == event)
        .collect();

    if matching.is_empty() {
        return;
    }

    let input = match &event {
        HookEvent::SessionEnd => HookInput {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: event.clone(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: reason.map(|r| r.to_string()),
            model: Some(current_model.to_string()),
            subagent_name: None,
            subagent_result: None,
            message_count: None,
        },
        HookEvent::PreCompact | HookEvent::PostCompact => HookInput::compact(
            session_id,
            transcript_path,
            cwd,
            event.clone(),
            message_count.unwrap_or(0),
        ),
        HookEvent::Notification => HookInput {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: event.clone(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: None,
            model: Some(current_model.to_string()),
            subagent_name: None,
            subagent_result: None,
            message_count: None,
        },
        _ => return,
    };

    for registered in matching {
        if let Some(ref msg) = registered.hook.get_status_message() {
            tracing::info!(
                plugin = %registered.plugin_name,
                event = ?event,
                "Hook status: {}",
                msg
            );
        }

        if registered.hook.is_async() {
            // Fire-and-forget async hook
            let hook = registered.hook.clone();
            let input = input.clone();
            let registered = registered.clone();
            tokio::spawn(async move {
                let _ = match &hook {
                    HookType::Command { .. } => {
                        execute_command_hook(&hook, &input, &registered).await
                    }
                    HookType::Http { .. } => execute_http_hook(&hook, &input).await,
                    _ => HookAction::Allow,
                };
            });
            continue;
        }

        let _action = match &registered.hook {
            HookType::Command { .. } => {
                execute_command_hook(&registered.hook, &input, registered).await
            }
            HookType::Prompt { .. } => {
                // No LLM factory available in standalone context; skip
                HookAction::Allow
            }
            HookType::Http { .. } => execute_http_hook(&registered.hook, &input).await,
            HookType::Agent { .. } => {
                // No LLM factory available in standalone context; skip
                HookAction::Allow
            }
        };
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
