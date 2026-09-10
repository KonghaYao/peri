//! Lifecycle adapters and creation/resume intent for the Agent-owned factory.
use super::fire_subagent_lifecycle_hooks_static;
use crate::tool_search::ExecuteExtraToolResolver;
use peri_agent::session::subagent::{
    SessionFactory, SubagentLifecycleStart, SubagentLifecycleStop, SubagentResumeConfig,
    SubagentRunMode, SubagentSpawnConfig, SubagentSpawned,
};
use peri_agent::thread::ThreadStore;
use peri_agent::{agent::react::ReactLLM, messages::BaseMessage, tools::BaseTool};
use std::sync::Arc;

impl super::SubAgentTool {
    /// 生命周期 hook 闭包（middlewares 构造：内部触发 RegisteredHook；
    /// registered_hooks 为空时不构造闭包）。
    pub(crate) fn lifecycle_closures(
        &self,
    ) -> (
        Option<SubagentLifecycleStart>,
        Option<SubagentLifecycleStop>,
    ) {
        if self.registered_hooks.is_empty() {
            return (None, None);
        }
        let hooks_start = self.registered_hooks.clone();
        let on_subagent_start: Option<SubagentLifecycleStart> =
            Some(Arc::new(move |name: &str, cwd: &str| {
                let hooks = hooks_start.clone();
                let name = name.to_string();
                let cwd = cwd.to_string();
                tokio::spawn(async move {
                    fire_subagent_lifecycle_hooks_static(
                        &hooks,
                        crate::hooks::types::HookEvent::SubagentStart,
                        &cwd,
                        &name,
                        None,
                    )
                    .await;
                });
            }));
        let hooks_stop = self.registered_hooks.clone();
        let on_subagent_stop: Option<SubagentLifecycleStop> = Some(Arc::new(
            move |name: &str, cwd: &str, result: &str, is_error: bool| {
                let hooks = hooks_stop.clone();
                let name = name.to_string();
                let cwd = cwd.to_string();
                let result = result.to_string();
                tokio::spawn(async move {
                    fire_subagent_lifecycle_hooks_static(
                        &hooks,
                        crate::hooks::types::HookEvent::SubagentStop,
                        &cwd,
                        &name,
                        Some(&result),
                    )
                    .await;
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
        llm: Box<dyn ReactLLM + Send + Sync>,
        tools: Vec<Arc<dyn BaseTool>>,
        tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
        system_prompt: Option<String>,
        skill_names: Vec<String>,
        cwd: String,
    ) -> SubagentSpawnConfig {
        let host = self.host();
        let (on_subagent_start, on_subagent_stop) = self.lifecycle_closures();
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
            chain_assembler: Arc::clone(&self.chain_assembler),
            tools,
            tool_filter,
            system_prompt,
            error_suggest_registry: None,
            tool_registry_snapshot: None,
            tool_invocation_resolver: Some(Arc::new(ExecuteExtraToolResolver::default())),
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            thread_store: host.thread_store.clone(),
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
    /// [`Self::spawn_config_base`]：error_suggest_registry / tool_registry_snapshot /
    /// compact_config / context_budget / compact_llm 恒 None 与 spawn 一致；
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
        llm: Box<dyn ReactLLM + Send + Sync>,
        tools: Vec<Arc<dyn BaseTool>>,
        tool_filter: Arc<dyn Fn(&str) -> bool + Send + Sync>,
        thread_store: Arc<dyn ThreadStore>,
        cwd: String,
    ) -> SubagentResumeConfig {
        let host = self.host();
        let (on_subagent_start, on_subagent_stop) = self.lifecycle_closures();
        SubagentResumeConfig {
            thread_id,
            prompt,
            agent_name: None,
            run_mode,
            max_iterations,
            llm,
            chain_assembler: Arc::clone(&self.chain_assembler),
            tools,
            tool_filter,
            tool_invocation_resolver: Some(Arc::new(ExecuteExtraToolResolver::default())),
            error_suggest_registry: None,
            tool_registry_snapshot: None,
            compact_config: None,
            context_budget: None,
            compact_llm: None,
            thread_store,
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
