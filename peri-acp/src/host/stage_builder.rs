//! ACP 侧 stage 装配投影层（L5：executor 拆分后保留的 ACP 装配面薄壳）。
//!
//! 执行本体已随 L5 物理迁入 peri-agent：
//! `peri_agent::session::exec::stage_builder`（`build_agent` /
//! `build_stage_context` / `V2AgentOutput` / `CachedLlmInstances` /
//! `StageBuildInput`）。本模块不再持有装配实现，只做两件事：
//!
//! 1. 从投影型 [`SessionContext`]（`peri-agent::session::exec::executor`）投影
//!    构造 [`StageBuildInput`]（会话数据逐字段对应；注入面按原 ACP 语义构造）；
//! 2. 经注入参数接入装配器（`MiddlewareChainAssembler`，宿主传
//!    `ProductionChainAssembler`）与 compact hook 闭包，调用 peri_agent 正式
//!    `build_stage_context`，**透传**其 `V2AgentOutput`（本地不再定义同名类型）。
//!
//! 注入面（原 ACP 特有构造）在此构造：
//! - `render_system_prompt`：agent overrides 主 prompt 覆盖渲染（workflow
//!   feature 与 WorkflowTool 条件注册同源——`workflow_executor.is_some()`）；
//! - `system_builder`：SubAgent system prompt 构建器（无 workflow feature +
//!   frozen date）；
//! - `compact_pre_hook` / `compact_post_hook`：经注入参数接入（宿主
//!   host/prompt.rs `build_compact_hooks` 构造，本模块不再承载）；
//! - `langfuse_bridge_factory`：由宿主 turn 级 Langfuse hooks 捕获后传入；
//! - `shared_queue` / `idle_inbox` / `launch_cron_bridge`：经
//!   [`SessionAccessPort`] 定位（原 `SessionManager` 路径；None = print mode /
//!   无 session，走 turn 级 CronOwner 路径）。
//!
//! 依赖方向（§0）：本模块只依赖 peri-acp-types / peri-agent（执行面）/
//! crate 内部（prompt 渲染）。装配器与 compact hook fire 动作经注入参数接入，
//! 本模块不再引用 middlewares。

use std::sync::Arc;

use peri_acp_types::{
    agents::AgentOverrides,
    event::AgentEventHandler,
    frozen::{ChildHandlerFactory, FrozenData, ThreadPersistence},
    goal::GoalController,
    session::SessionInbox,
};
use peri_agent::agent::{async_tasks::TaskManager, LangfuseBridgeLike};
use peri_agent::session::exec::stage_builder::{
    build_stage_context as build_stage_context_agent, CachedLlmInstances, StageBuildInput,
    V2AgentOutput,
};
// 装配器与注入闭包类型事实源在 peri-agent（middlewares 侧仅 re-export，
// 本模块直接引事实源，不触碰 middlewares）
use peri_agent::session::factory::{
    AssemblyContext, ChainAssembly, MiddlewareChainAssembler, OnBgCompleteFn, SystemPromptBuilder,
};

use crate::prompt::{PromptEnv, PromptFeatures, PromptTemplate};

/// 从投影型 [`SessionContext`] 构造 [`StageBuildInput`] 并调用 peri_agent 正式
/// `build_stage_context`（stage 装配本体，`peri-agent/src/session/exec/stage_builder.rs`）。
///
/// 签名保持 ACP 装配面形态（18 参数）：前 17 个与正式签名一一对应
/// （`assembler` 为链装配器注入——宿主传 `ProductionChainAssembler`；
/// `compact_pre_hook` / `compact_post_hook` 为 compact hook 闭包注入——宿主
/// `host/prompt.rs build_compact_hooks` 构造），第 18 个 `langfuse_bridge_factory`
/// 为 ACP 特有注入面（宿主 turn 级 Langfuse hooks 构造的闭包工厂；正式实现的
/// Langfuse bridge 经 `StageBuildInput::langfuse_bridge_factory` 消费）。
///
/// 返回 peri_agent 的 [`V2AgentOutput`]（透传；`StageBuildFn` 契约类型）。
///
/// `auxiliary_model` 为 `StageBuildFn` 契约镜像签名（`StageBuildRequest.
/// auxiliary_model` 同型透传）：经 `peri_acp_types::model::Model` 引用
/// （re-export，非直接持有 `peri_model` 路径），类型与 peri-agent 正式
/// `build_stage_context` 参数一致。
///
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn build_stage_context(
    ctx: &crate::session::executor::SessionContext,
    assembler: &dyn MiddlewareChainAssembler<Context = AssemblyContext, Output = ChainAssembly>,
    compact_pre_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    compact_post_hook: Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
    cached_llm: Option<&CachedLlmInstances>,
    system_prompt: String,
    subagent_system_prompt: Option<String>,
    frozen: FrozenData,
    event_handler: Arc<dyn AgentEventHandler>,
    agent_overrides: Option<AgentOverrides>,
    preload_skills: Vec<String>,
    child_handler_factory: Option<ChildHandlerFactory>,
    auxiliary_model: Option<Arc<dyn peri_acp_types::model::Model>>,
    thread_persistence: ThreadPersistence,
    goal_controller: Option<Arc<dyn GoalController>>,
    task_manager: Option<Arc<TaskManager>>,
    on_bg_complete: Option<OnBgCompleteFn>,
    langfuse_bridge_factory: Option<Arc<dyn Fn() -> Arc<dyn LangfuseBridgeLike> + Send + Sync>>,
) -> (V2AgentOutput, Option<CachedLlmInstances>) {
    // ── 会话级共享变量（原 session_manager 端口化；None = print mode）──
    let session_access = ctx.session_access.clone();
    // 会话级共享 v2 MessageQueue（每 turn 同一实例，跨 turn 存活）
    let shared_queue = session_access
        .as_ref()
        .and_then(|sa| sa.v2_message_queue(&ctx.session_id))
        .unwrap_or_default();
    // 会话级 SessionInbox（await-wake 路径；allow_await_wake 由宿主装配面判定）
    let idle_inbox: Option<Arc<SessionInbox>> = if ctx.allow_await_wake {
        session_access
            .as_ref()
            .and_then(|sa| sa.session_inbox(&ctx.session_id))
    } else {
        None
    };
    // 会话级 idle-suspended 标志（与 idle_inbox 同源注入；await_wake 挂起期间
    // executor 置 true，宿主 dispatch_prompt_turn 据此把挂起期间到达的用户
    // prompt 注入 inbox 唤醒 loop，而非在 prompt lock 上阻塞）。
    let idle_suspended_flag: Option<Arc<std::sync::atomic::AtomicBool>> = if ctx.allow_await_wake {
        session_access
            .as_ref()
            .and_then(|sa| sa.idle_suspended_flag(&ctx.session_id))
    } else {
        None
    };
    // session 级 cron bridge 惰性启动器（SessionManager 路径；无则走
    // print 模式 turn 级 CronOwner——正式实现内部处理）
    let launch_cron_bridge: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>> =
        session_access.map(|sa| {
            Arc::new(move |sid: &str| sa.cron_bridge_for(sid))
                as Arc<dyn Fn(&str) -> bool + Send + Sync>
        });

    // ── 注入面：主 prompt 覆盖渲染（agent overrides 非空时调用）──
    // workflow_enabled 与正式实现 WorkflowMiddlewareAdaptor 条件注册共用同一
    // 条件源（workflow_executor.is_some()），保证 prompt 声明与工具注册一致。
    let render_system_prompt: Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync> = {
        let permission_mode = Arc::clone(&ctx.permission_mode);
        let workflow_enabled = ctx.workflow_executor.is_some();
        let skills = Arc::clone(&ctx.skills);
        let plugin_agent_dirs = ctx.plugin_agent_dirs.clone();
        Arc::new(move |ov: Option<&AgentOverrides>, cwd: &str| {
            let features = PromptFeatures::detect(permission_mode.load(), workflow_enabled);
            let template = ov.map_or_else(PromptTemplate::new, PromptTemplate::with_overrides);
            let env = PromptEnv::detect(cwd);
            template.render(&env, &features, skills.as_ref(), &plugin_agent_dirs, None)
        })
    };

    // ── 注入面：SubAgent system prompt 构建器 ──
    // PromptFeatures::detect_without_workflow（subagent / fork 链不注册
    // WorkflowTool，不得宣称 workflow 可用）；frozen date / language 注入。
    let system_builder: SystemPromptBuilder = {
        let frozen_date_for_sub = frozen.date.clone();
        let frozen_language_for_sub = ctx.language.clone();
        let skills_for_sub = Arc::clone(&ctx.skills);
        let features_for_sub = PromptFeatures::detect_without_workflow(ctx.permission_mode.load());
        let template_for_sub = PromptTemplate::new();
        Arc::new(move |overrides: Option<&AgentOverrides>, cwd_dir: &str| {
            let t =
                overrides.map_or_else(|| template_for_sub.clone(), PromptTemplate::with_overrides);
            let env = if let Some(ref date) = frozen_date_for_sub {
                PromptEnv::with_frozen_date(cwd_dir, date)
            } else {
                PromptEnv::detect(cwd_dir)
            };
            t.render(
                &env,
                &features_for_sub,
                skills_for_sub.as_ref(),
                &[],
                frozen_language_for_sub.as_deref(),
            )
        })
    };

    // ── 注入面：compact plugin hook 回调（宿主 host/prompt.rs
    //    build_compact_hooks 构造，经参数接入；语义同迁移前 stage_builder：
    //    tokio::spawn 转发 fire_pre_compact / fire_post_compact，不阻塞管线）──

    // ── StageBuildInput 投影（会话数据逐字段对应 + 注入面）──
    let input = StageBuildInput {
        // 会话数据
        cwd: ctx.cwd.clone(),
        session_id: ctx.session_id.clone(),
        cancel: ctx.cancel.clone(),
        broker: Arc::clone(&ctx.broker),
        permission_mode: Arc::clone(&ctx.permission_mode),
        plugin_skill_roots: ctx.plugin_skill_roots.clone(),
        plugin_loaded: ctx.plugin_loaded.clone(),
        hook_groups: ctx.hook_groups.clone(),
        session_start_source: ctx.session_start_source.clone(),
        cron_scheduler: ctx.cron_scheduler.clone(),
        mcp_pool: ctx.mcp_pool.clone(),
        channel_state: ctx.channel_state.clone(),
        tool_search_index: Arc::clone(&ctx.tool_search_index),
        shared_tools: Arc::clone(&ctx.shared_tools),
        lsp_servers: ctx.lsp_servers.clone(),
        lsp_pool: ctx.lsp_pool.clone(),
        workflow_executor: ctx.workflow_executor.clone(),
        workflow_middleware: ctx.workflow_middleware.clone(),
        thread_store: ctx.thread_store.clone(),
        thread_id: ctx.thread_id.clone(),
        // 注入面
        model_name: ctx.provider_model_name.clone(),
        provider_name: ctx.provider_name.clone(),
        context_window: ctx.effective_context_window,
        claude_md_excludes: ctx.claude_md_excludes.clone().unwrap_or_default(),
        language: ctx.language.clone(),
        compact_config: ctx.compact_config.clone(),
        retry_events: ctx
            .retry_events
            .as_ref()
            .map(|r| (**r).clone())
            .unwrap_or_default(),
        // LLM 构造闭包：宿主装配面（host/prompt.rs / host/stdio/...）构造，
        // 生产路径恒 Some（None 仅防御，不掩盖装配面缺失）。
        primary_llm_factory: ctx
            .primary_llm_factory
            .clone()
            .expect("stage projection: primary_llm_factory 必须由宿主装配面构造"),
        auto_classifier_factory: ctx
            .auto_classifier_factory
            .clone()
            .expect("stage projection: auto_classifier_factory 必须由宿主装配面构造"),
        llm_factory: ctx
            .subagent_llm_factory
            .clone()
            .expect("stage projection: subagent_llm_factory 必须由宿主装配面构造"),
        provider_fp: ctx.provider_fp.clone(),
        render_system_prompt,
        system_builder,
        langfuse_bridge_factory,
        shared_queue,
        idle_inbox,
        idle_suspended_flag,
        launch_cron_bridge,
        tool_invocation_resolver: Arc::clone(&ctx.tool_invocation_resolver),
        compact_pre_hook,
        compact_post_hook,
    };

    // 调用 peri_agent 正式 stage 装配本体（透传 V2AgentOutput）
    build_stage_context_agent(
        &input,
        assembler,
        cached_llm,
        system_prompt,
        subagent_system_prompt,
        frozen,
        event_handler,
        agent_overrides,
        preload_skills,
        child_handler_factory,
        auxiliary_model,
        thread_persistence,
        goal_controller,
        task_manager,
        on_bg_complete,
    )
}
