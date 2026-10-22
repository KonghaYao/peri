//! session 级交互装配：idle 准入规则链 + session 交互门的取用。
//!
//! 设计 §3.2 / root 裁决 R1：idle 规则链由 `production_blueprint()` **白名单
//! 派生**（[`invocation_rule_blueprint`]），经**同一** `ProductionChainAssembler`
//! 装配；`AssemblyContext` 取 session 级字段。本文件只构造上下文，不复制规则实现。
//!
//! 依赖方向：本文件只引用 `peri-agent`（上下文与派生入口）与 `peri-acp-types`
//! 端口；具体装配器（middlewares 侧的 `ProductionChainAssembler`）由调用方
//! （`host/workspace.rs`，装配白名单）以 `&A` 注入，因此本文件不需要 middlewares
//! 全路径豁免。

use std::collections::HashSet;
use std::sync::Arc;

use peri_acp_types::event::FnEventHandler;
use peri_acp_types::interaction::UserInteractionBroker;
use peri_acp_types::session_resources::SessionResources;
use peri_agent::agent::async_tasks::TaskManager;
use peri_agent::agent::AgentCancellationToken;
use peri_agent::session::factory::{
    build_invocation_rule_chain, AssemblyContext, ChainAssembly, InvocationRuleChain,
    MiddlewareChainAssembler,
};
use peri_agent::session::subagent::SubagentLlmSource;

use super::workspace::SessionEnvironment;
use super::AcpServerConfig;
use crate::provider::{LlmProvider, PeriConfig};

/// session-owned 交互门：环境自带（router 借用的同一 `Arc`）；无环境（无工作区
/// 的测试会话）时按既有语义新建一把——那时没有执行路径，门只提供形状。
pub(crate) fn session_interaction_gate(
    environment: &Option<Arc<SessionEnvironment>>,
) -> Arc<tokio::sync::Mutex<()>> {
    environment
        .as_ref()
        .map(|env| env.interaction_gate())
        .unwrap_or_default()
}

/// 装配 idle 准入规则链（`Hook` + `Permission` 派生的窄链）。
///
/// 上下文字段分三类：
///
/// 1. **派生蓝本真正消费的 session 级事实**（取自真实装配面）：
///    `hook_groups` / `permission_mode` / `provider_name` / `cwd` / `session_id` /
///    `broker`（session 交互路由）/ `meta_harness_disabled`（关闭集，与链槽跳过
///    判据同一份）/ `llm_factory`（hook 的 prompt 型 hook 用）/ `task_manager`
///    （**会话级**管理器：idle 期异步 hook 的 `spawn_owned` 与 command hook 的
///    `begin_external_execution` 因此进入会话任务作用域，随会话关闭一起 drain）。
/// 2. **其余槽位**（被派生蓝本排除，见 [`invocation_rule_blueprint`] 的排除表）：
///    传 session 级事实或显式 `None`，不伪造 per-turn 快照。
/// 3. **`AssemblyContext` 形状必填但派生蓝本触达不到的端口**：`event_handler` /
///    `bg_event_tx` / `todo_tx`。它们只在被排除的槽位（SubAgent / Todo）里被消费，
///    而装配器在蓝本过滤之前会构造 SubAgent 中间件实例（随后随产物丢弃）——
///    这里给的是**无副作用**端口，注释即凭据；不得用它冒充可用的模型事件通道。
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_idle_invocation_rules<A>(
    assembler: &A,
    cfg: &AcpServerConfig,
    provider: &LlmProvider,
    peri_config: &Arc<PeriConfig>,
    session_id: &str,
    cwd: &str,
    broker: Arc<dyn UserInteractionBroker>,
    cancel: AgentCancellationToken,
    disabled_middlewares: &HashSet<String>,
    agent_default_run_in_background: bool,
    session_resources: &Arc<dyn SessionResources>,
    session_task_manager: &Arc<dyn peri_acp_types::tasks::TaskManager>,
) -> InvocationRuleChain
where
    A: MiddlewareChainAssembler<Context = AssemblyContext, Output = ChainAssembly>,
{
    // hook 的独立 LLM 消费者（prompt 型 hook）：按 session provider 快照解析，
    // 无子链身份（与 turn 路径的 `into_plain_bridge` 语义一致；此处不做池缓存，
    // idle 入口不参与 per-turn 缓存生命周期）。
    let llm_factory = {
        let provider = provider.clone();
        let peri_config = Arc::clone(peri_config);
        let session_id = session_id.to_owned();
        Arc::new(move |alias: Option<&str>| {
            let resolved =
                alias.and_then(|alias| LlmProvider::from_config_for_alias(&peri_config, alias));
            let (model_provider, model_name) = match resolved {
                Some(resolved) => {
                    let model_name = resolved.model_name().to_string();
                    (resolved, model_name)
                }
                None => (provider.clone(), provider.model_name().to_string()),
            };
            SubagentLlmSource::model(model_provider.into_model().into(), model_name)
                .with_session_id(session_id.clone())
        })
    };

    // auto-classifier 端口（Permission 的自动分类分支）：session provider 快照的
    // 真实模型；idle 入口不装 per-turn 观测。
    let auto_classifier_model = Arc::new(tokio::sync::Mutex::new(provider.clone().into_model()));

    // 会话级后台任务管理器：**还原具体类型**（`AssemblyContext` 只接受具体实现）。
    //
    // 失败即显式告警 + no-op（`without_session_scope`：作用域已关闭，任何后台接管
    // 都被拒绝）：不得静默掩盖，也**不得**回落游离实例——游离实例的 `spawn_owned`
    // 不随会话 drain，正是本处要修的缺口。
    let task_manager: Arc<TaskManager> = {
        let manager = Arc::clone(session_task_manager);
        // trait upcasting：`TaskManager`（端口）→ `Arc<dyn Any + Send + Sync>` → 具体类型。
        let any: Arc<dyn std::any::Any + Send + Sync> =
            manager as Arc<dyn std::any::Any + Send + Sync>;
        match any.downcast::<TaskManager>() {
            Ok(concrete) => concrete,
            Err(_) => {
                tracing::warn!(
                    session_id,
                    "idle invocation rules: session task manager is not peri_agent::TaskManager; \
                     idle background hooks are rejected (no detached fallback)"
                );
                Arc::new(TaskManager::without_session_scope())
            }
        }
    };

    // 派生蓝本触达不到的形状端口：
    // - event_handler：仅 SubAgent 中间件构造需要（SubAgent ∉ 蓝本 ⇒ 不进链、不可达）；
    // - bg_event_tx / todo_tx：仅 SubAgent / Todo 槽位消费（均不在蓝本内）。
    let (bg_event_tx, _bg_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel(1);

    let ctx = AssemblyContext {
        cwd: cwd.to_owned(),
        cancel,
        broker,
        permission_mode: Arc::clone(&cfg.permission_mode),
        model_name: provider.model_name().to_string(),
        provider_name: provider.display_name().to_string(),
        auxiliary_model: None,
        auto_classifier_model,
        preload_skills: Vec::new(),
        plugin_skill_roots: cfg.plugin_skill_roots.clone(),
        agent_catalog: Arc::clone(&cfg.agent_catalog),
        plugin_loaded: cfg.plugin_loaded.clone(),
        hook_groups: cfg.hook_groups.clone(),
        // idle 入口不是会话启动路径：不注入启动来源（hook 的 SessionStart 语义
        // 由真实 turn 路径承载）。
        session_start_source: None,
        mcp_skill_registry: None,
        command_registry: None,
        cron_scheduler: cfg.cron_scheduler.clone(),
        mcp_pool: cfg.mcp_pool.clone(),
        session_id: session_id.to_owned(),
        tool_search_index: Arc::clone(&cfg.tool_search_index),
        shared_tools: Arc::clone(&cfg.shared_tools),
        // workflow 完成通知只服务 workflow 工具面（不在蓝本内）。
        event_handler: Arc::new(FnEventHandler(|_event| {})),
        // 会话级管理器（与 `SessionEnvironment::task_manager` 同一实例）：
        // 后台 hook 因此进入会话任务作用域，随会话关闭 drain。
        task_manager,
        bg_event_tx,
        on_bg_complete: None,
        langfuse_bridge: None,
        session_resources: Some(Arc::clone(session_resources)),
        parent_thread_id: None,
        register_runtime: None,
        deregister_runtime: None,
        child_handler_factory: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        system_prompt_for_sub: String::new(),
        llm_factory,
        system_builder: Arc::new(|_overrides, _cwd| String::new()),
        todo_tx,
        goal_controller: None,
        meta_harness_disabled: disabled_middlewares.clone(),
        agent_default_run_in_background,
        agent_overrides: None,
        language: None,
    };

    build_invocation_rule_chain(assembler, &ctx)
}
