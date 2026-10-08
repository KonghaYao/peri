//! Lifecycle adapters and creation/resume intent for the Agent-owned factory.
use crate::tool_search::ExecuteExtraToolResolver;
use peri_acp_types::session_resources::SessionResources;
use peri_agent::session::subagent::{
    SessionFactory, SubagentLifecycleStart, SubagentLifecycleStop, SubagentLlmSource,
    SubagentResumeConfig, SubagentRunMode, SubagentSpawnConfig, SubagentSpawned,
};
use peri_agent::{messages::BaseMessage, tools::BaseTool};
use std::sync::Arc;

/// 生命周期 hook 默认非阻断：非 Allow action 只做无正文诊断，不影响子 agent。
fn record_subagent_lifecycle_action(
    event: &crate::hooks::types::HookEvent,
    name: &str,
    action: &crate::hooks::types::HookAction,
) {
    use crate::hooks::types::HookAction;
    let kind = match action {
        HookAction::Allow => return,
        HookAction::Block { .. } => "block",
        HookAction::PreventContinuation { .. } => "prevent_continuation",
        HookAction::ModifyInput { .. } => "modify_input",
        HookAction::PermissionOverride { .. } => "permission_override",
        HookAction::SystemMessage { .. } => "system_message",
        HookAction::AdditionalContext { .. } => "additional_context",
        HookAction::InitialUserMessage { .. } => "initial_user_message",
    };
    tracing::debug!(
        event = ?event,
        subagent = name,
        action = kind,
        "Subagent lifecycle hook returned a non-blocking action (ignored by design)"
    );
}

impl super::SubAgentTool {
    /// 生命周期 hook 分发器（懒建、按工具/会话共享一次）。
    ///
    /// [TRAP] 共享是 once 语义的前提：`once:true` 的 SubagentStart/Stop 必须跨
    /// 同一工具的多次 spawn/resume 只触发一次；按 spawn 新建 dispatcher 会让
    /// once 随每个子 agent 重新触发。
    pub(crate) fn lifecycle_dispatcher(
        &self,
    ) -> Option<Arc<crate::hooks::dispatcher::HookDispatcher>> {
        self.lifecycle_dispatcher
            .get_or_init(|| {
                if self.registered_hooks.is_empty() {
                    return None;
                }
                Some(Arc::new(
                    crate::hooks::dispatcher::HookDispatcher::new_without_llm(
                        self.registered_hooks.to_vec(),
                        Arc::new(crate::hooks::once_tracker::OnceTracker::new()),
                        self.parent_cwd.clone(),
                    )
                    .with_task_manager_opt(
                        self.host()
                            .task_manager
                            .clone()
                            .map(|manager| manager as Arc<dyn peri_acp_types::tasks::TaskManager>),
                    ),
                ))
            })
            .clone()
    }

    /// 生命周期 hook 闭包（middlewares 构造）：统一经 [`HookDispatcher`] 分发
    /// （matcher / if 条件 / once / async spawn / 超时 / 取消 / 进程树 owner）。
    /// registered_hooks 为空时不构造闭包。
    ///
    /// 默认非阻断：action 仅记录诊断，不阻断子 agent。
    /// [`HookDispatcher`]: crate::hooks::dispatcher::HookDispatcher
    pub(crate) fn lifecycle_closures(
        &self,
    ) -> (
        Option<SubagentLifecycleStart>,
        Option<SubagentLifecycleStop>,
    ) {
        let Some(dispatcher) = self.lifecycle_dispatcher() else {
            return (None, None);
        };
        let start_dispatcher = Arc::clone(&dispatcher);
        let on_subagent_start: Option<SubagentLifecycleStart> =
            Some(Arc::new(move |name: &str, cwd: &str| {
                let dispatcher = Arc::clone(&start_dispatcher);
                let name = name.to_string();
                let cwd = cwd.to_string();
                tokio::spawn(async move {
                    use crate::hooks::types::{HookEvent, HookInput};
                    let mut input = HookInput::subagent_start("", "", &cwd, &name);
                    // 真实身份：agent_type = 子 agent 名（agent_id 由 peri-agent 回调
                    // 扩展接入，作为交接项）。
                    input.agent_type = Some(name.clone());
                    let action = dispatcher
                        .fire_subagent_lifecycle(HookEvent::SubagentStart, &input, &name)
                        .await;
                    record_subagent_lifecycle_action(&HookEvent::SubagentStart, &name, &action);
                });
            }));
        let stop_dispatcher = Arc::clone(&dispatcher);
        let on_subagent_stop: Option<SubagentLifecycleStop> = Some(Arc::new(
            move |name: &str, cwd: &str, result: &str, is_error: bool| {
                let dispatcher = Arc::clone(&stop_dispatcher);
                let name = name.to_string();
                let cwd = cwd.to_string();
                let result = result.to_string();
                tokio::spawn(async move {
                    use crate::hooks::types::{HookEvent, HookInput};
                    let mut input = HookInput::subagent_stop("", "", &cwd, &name, &result);
                    input.agent_type = Some(name.clone());
                    let action = dispatcher
                        .fire_subagent_lifecycle(HookEvent::SubagentStop, &input, &name)
                        .await;
                    record_subagent_lifecycle_action(&HookEvent::SubagentStop, &name, &action);
                });
                let _ = is_error; // SubagentStop hook 不区分 error/正常
            },
        ));
        (on_subagent_start, on_subagent_stop)
    }

    /// 组装 [`SubagentSpawnConfig`](peri_agent::session::subagent::SubagentSpawnConfig) 的公共部分（父侧通道 + 意图骨架）。
    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    pub(crate) fn spawn_config_base(
        &self,
        agent_name: String,
        prompt: String,
        parent_messages: Vec<BaseMessage>,
        cancel_policy: peri_agent::session::subagent::SubagentCancelPolicy,
        max_iterations: usize,
        fork_directive_kind: Option<peri_agent::session::subagent::ForkDirectiveKind>,
        run_mode: peri_agent::session::subagent::SubagentRunMode,
        llm: SubagentLlmSource,
        tools: Vec<Arc<dyn BaseTool>>,
        tool_filter: peri_agent::session::tool_catalog::ToolFilter,
        system_prompt: Option<String>,
        skill_names: Vec<String>,
        cwd: String,
        parent_tool_call_id: Option<String>,
    ) -> SubagentSpawnConfig {
        let host = self.host();
        let (on_subagent_start, on_subagent_stop) = self.lifecycle_closures();
        let inherited_filter = Arc::clone(&self.inherited_tool_filter);
        let tool_filter =
            Arc::new(move |tool: &dyn BaseTool| inherited_filter(tool) && tool_filter(tool));
        SubagentSpawnConfig {
            agent_name,
            prompt,
            parent_messages,
            cancel_policy,
            max_iterations,
            fork_directive_kind,
            run_mode,
            skill_names,
            llm,
            chain_assembler: Arc::new(super::session_binding::SessionBoundAssembler::new(self)),
            tools,
            tool_filter,
            system_prompt,
            tool_invocation_resolver: Some(Arc::new(ExecuteExtraToolResolver::default())),
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            session_resources: host.session_resources.clone(),
            event_handler: self.event_handler.clone(),
            bg_event_sender: host.bg_event_sender.clone(),
            task_manager: host.task_manager.clone(),
            on_bg_complete: host.on_bg_complete.clone(),
            langfuse_bridge: host.langfuse_bridge.clone(),
            on_subagent_start,
            on_subagent_stop,
            register_runtime: host.register_runtime.clone(),
            deregister_runtime: host.deregister_runtime.clone(),
            parent_agent_id: *self.parent_agent_id.read(),
            parent_tool_call_id,
            // 父侧数据回退（parent session 存在时由 spawn_subagent 覆盖）
            cancel_token: self.cancel.clone(),
            cwd: Some(cwd),
            parent_thread_id: host.parent_thread_id.clone(),
            frozen_claude_md: host.frozen_claude_md.as_deref().map(|s| s.to_string()),
            frozen_claude_local_md: host
                .frozen_claude_local_md
                .as_deref()
                .map(|s| s.to_string()),
            frozen_skill_summary: host.frozen_skill_summary.as_deref().map(|s| s.to_string()),
            frozen_date: None,
        }
    }

    /// 调用统一入口（parent 存在时 frozen/thread 父子链自 parent session 读取）。
    pub(crate) async fn spawn(
        &self,
        config: SubagentSpawnConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        let parent = self.parent_session.read().clone();
        SessionFactory::spawn_subagent(parent.as_ref(), config).await
    }
    /// 组装 [`SubagentResumeConfig`](peri_agent::session::subagent::SubagentResumeConfig) 公共部分（通道段逐字段对照
    /// [`Self::spawn_config_base`]：compact_config / context_budget / compact_llm 恒 None 与 spawn 一致；
    /// `tool_invocation_resolver: Some(ExecuteExtraToolResolver::default())`
    /// 显式设置保持包装层语义，R2 补充）。
    ///
    /// `agent_name` 恒传 None——由 agent 层从 `meta.title` 取（R2 补充：避免
    /// 双源；thread 创建时 title 已固化 = spawn 时的 agent_name）。
    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    pub(crate) fn resume_config_base(
        &self,
        thread_id: String,
        prompt: Option<String>,
        run_mode: SubagentRunMode,
        max_iterations: usize,
        llm: SubagentLlmSource,
        tools: Vec<Arc<dyn BaseTool>>,
        tool_filter: peri_agent::session::tool_catalog::ToolFilter,
        session_resources: Arc<dyn SessionResources>,
        cwd: String,
        parent_tool_call_id: Option<String>,
    ) -> SubagentResumeConfig {
        let host = self.host();
        let (on_subagent_start, on_subagent_stop) = self.lifecycle_closures();
        let inherited_filter = Arc::clone(&self.inherited_tool_filter);
        let tool_filter =
            Arc::new(move |tool: &dyn BaseTool| inherited_filter(tool) && tool_filter(tool));
        SubagentResumeConfig {
            thread_id,
            prompt,
            agent_name: None,
            run_mode,
            max_iterations,
            llm,
            chain_assembler: Arc::new(super::session_binding::SessionBoundAssembler::new(self)),
            tools,
            tool_filter,
            tool_invocation_resolver: Some(Arc::new(ExecuteExtraToolResolver::default())),
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            session_resources,
            event_handler: self.event_handler.clone(),
            bg_event_sender: host.bg_event_sender.clone(),
            task_manager: host.task_manager.clone(),
            on_bg_complete: host.on_bg_complete.clone(),
            langfuse_bridge: host.langfuse_bridge.clone(),
            on_subagent_start,
            on_subagent_stop,
            register_runtime: host.register_runtime.clone(),
            deregister_runtime: host.deregister_runtime.clone(),
            parent_agent_id: *self.parent_agent_id.read(),
            parent_tool_call_id,
            // 父侧数据回退（parent session 存在时由 resume_subagent 覆盖）
            cancel_token: self.cancel.clone(),
            cwd: Some(cwd),
            frozen_claude_md: host.frozen_claude_md.as_deref().map(|s| s.to_string()),
            frozen_claude_local_md: host
                .frozen_claude_local_md
                .as_deref()
                .map(|s| s.to_string()),
            frozen_skill_summary: host.frozen_skill_summary.as_deref().map(|s| s.to_string()),
            frozen_date: None,
        }
    }

    /// 调用统一恢复入口（parent 存在时 frozen copy 自 parent session 读取；
    /// 与 [`Self::spawn`] 同款包装，parent 链校验由 Agent 层恢复入口负责）。
    pub(crate) async fn resume(
        &self,
        config: SubagentResumeConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        let parent = self.parent_session.read().clone();
        SessionFactory::resume_subagent(parent.as_ref(), config).await
    }
}
