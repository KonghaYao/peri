//! 子 Agent 创建统一入口（3.0 L3 迁移）。
//!
//! L3 归位：subagent 创建逻辑（建 thread / 建 session / 运行 + 收尾）自
//! `peri-middlewares/src/subagent/`（spawner / execute_fork / execute_bg /
//! build_agent 四条路径 + ACP `/bg` 命令）收敛至 [`spawn_subagent`]。
//! Middleware 只声明工具与发起意图（组装 [`SubagentSpawnConfig`]），
//! 不持有创建实现。
//!
//! 依赖方向：Agent 层不反向依赖 middlewares。子链装配经
//! [`SubagentChainAssembler`] trait 依赖反转（中间件层提供实现，
//! 链序 AgentsMd→Skills→[SkillPreload]→Todo 由实现方保持，ARC-MIDDLEWARE-001）；
//! 生命周期 hook 触发经 [`SubagentLifecycleStart`]/[`SubagentLifecycleStop`]
//! 闭包注入（middlewares 构造闭包，内部触发其 RegisteredHook）。
//!
//! 验收语义：
//! - subagent 必有持久化 thread（parent_thread_id 父子链；transcript 绑定
//!   `with_persistence`，thread_id = agent_id）；
//! - frozen data 从父 session copy（parent 为 Some 时 claude_md / skill_summary /
//!   date 取自 `parent.store().frozen`，不重新读取磁盘）；
//! - agent_status 收尾语义与迁移前一致：done / cancelled / error。

use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_acp_types::thread::CancelPolicy;
use tokio_util::sync::CancellationToken;

use crate::agent::async_tasks::{
    BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind, TaskManager,
};
use crate::agent::events::{AgentEventHandler, ExecutorEvent};
use crate::agent::events_v2::{
    observe_event_to_executor, EventBus, EventBusConfig, EventHandles, ObserveEvent,
};
use crate::agent::react::{AgentOutput, ReactLLM};
use crate::agent::stages::{run_react_loop, LoopResult, SharedToolMap, StageContext};
use crate::agent::subagent_event_forwarder::spawn_subagent_event_forwarder;
use crate::agent::{CompactConfig, ContextBudget, LangfuseBridgeLike};
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::messages::BaseMessage;
use crate::middleware::chain::MiddlewareChain;
use crate::session::factory::{DeregisterRuntimeFn, RegisterRuntimeFn};
use crate::session::queue::{MessageKind, MessageSource, QueuedMessage};
use crate::session::turn::TurnId;
use crate::session::{FrozenContext, MessageQueue, Session};
use crate::thread::{ThreadMeta, ThreadStore};
use crate::tools::DirectToolInvocationResolver;
use crate::tools::{BaseTool, ToolInvocationResolver};

// ─── 意图类型 ────────────────────────────────────────────────────────────────

/// Fork 指令类型，决定 fork agent 使用的 system directive 模板
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkDirectiveKind {
    /// 使用 [`build_fork_directive`]（英文，Agent 工具路径）
    Fork,
    /// 使用 [`build_bg_fork_directive`]（中文，/bg 命令路径）
    Bg,
}

/// subagent 取消策略（与 ThreadMeta.cancel_policy 强类型对齐）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentCancelPolicy {
    /// Parent cancel → child cancel（同步 fork / 同步 agent 定义）
    Cascade,
    /// 仅 session 级 cancel_all_agents 可停止（后台）
    Independent,
}

impl SubagentCancelPolicy {
    fn as_cancel_policy(self) -> CancelPolicy {
        match self {
            Self::Cascade => CancelPolicy::Cascade,
            Self::Independent => CancelPolicy::Independent,
        }
    }
}

/// 运行模式：同步（当前 turn 内跑完）或后台（tokio::spawn + TaskManager 注册）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentRunMode {
    Sync,
    Background,
}

/// 子 agent 生命周期 hook 触发闭包（middlewares 构造，内部触发 RegisteredHook）。
/// 参数：(agent_name, cwd)。
pub type SubagentLifecycleStart = Arc<dyn Fn(&str, &str) + Send + Sync>;
/// 参数：(agent_name, cwd, result, is_error)。
pub type SubagentLifecycleStop = Arc<dyn Fn(&str, &str, &str, bool) + Send + Sync>;

// ─── 子链装配（依赖反转，ARC-MIDDLEWARE-001） ───────────────────────────────

/// 子 agent 链装配上下文：frozen 数据由 [`spawn_subagent`] 从父 session copy 后注入。
#[derive(Debug, Clone, Default)]
pub struct SubagentChainContext {
    /// 工作目录（解析 skill 文件路径）
    pub cwd: String,
    /// 需要预加载的 skill 名称（空 = 跳过 SkillPreloadMiddleware）
    pub skill_names: Vec<String>,
    /// Frozen CLAUDE.md/AGENTS.md main content（父 session copy；None = 从磁盘读取）
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content（上层注入的冻结数据）
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary（父 session copy）
    pub frozen_skill_summary: Option<String>,
}

/// 子 agent 中间件链装配器：由中间件层提供实现。
///
/// 链序（AgentsMd→Skills→[SkillPreload]→Todo）是行为契约，实现方必须保持
/// `peri-middlewares/src/subagent/tool/mod.rs` 的 `build_subagent_middlewares`
/// 顺序（ARC-MIDDLEWARE-001）。
pub trait SubagentChainAssembler: Send + Sync {
    fn assemble(&self, ctx: &SubagentChainContext) -> MiddlewareChain;
}

// ─── 父侧运行时宿主 ──────────────────────────────────────────────────────────

/// 父侧运行时通道聚合（L3）：executor/builder 在主 session 创建后注入，
/// subagent 创建所需的运行时通道统一经此读取，不再逐字段透传
/// SubAgentMiddleware。
#[derive(Clone, Default)]
#[allow(clippy::type_complexity)]
pub struct SubagentHost {
    /// 线程持久化存储（生产路径非 None；None 仅测试/遗留路径，跳过落库）
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 后台任务管理器（per-session 聚合）
    pub task_manager: Option<Arc<TaskManager>>,
    /// 后台任务完成事件通道（bg pump，独立于主 event pump）
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// bg 完成同步回调（registry.complete 之前调用，推送 Defer 到主 agent MQ）
    pub on_bg_complete:
        Option<Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    /// 子 agent 启动注册回调（active_agents）
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// 子 agent 结束注销回调
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
    /// Langfuse bridge（subagent trace）
    pub langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    /// Frozen CLAUDE.local.md（父 session 冻结数据中唯一不在 FrozenContext 的字段）
    pub frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen system prompt（fork 路径复用以避免重建；父 session 冻结的 subagent 版本）
    pub frozen_system_prompt: Option<Arc<String>>,
    /// 父线程 ID 回退值（生产路径由 spawn_subagent 从 parent session 读取）
    pub parent_thread_id: Option<String>,
    /// Frozen CLAUDE.md 回退值（生产路径由 spawn_subagent 从 parent session copy）
    pub frozen_claude_md: Option<Arc<String>>,
    /// Frozen skills summary 回退值（生产路径由 spawn_subagent 从 parent session copy）
    pub frozen_skill_summary: Option<Arc<String>>,
}

// ─── spawn 配置与产物 ────────────────────────────────────────────────────────

/// 子 agent 创建意图 + 装配产物 + 运行时通道（统一入口 [`spawn_subagent`] 的输入）。
///
/// 父侧数据（cwd / parent_thread_id / frozen claude_md / skill_summary / date /
/// cascade cancel token）在 `parent` 存在时从 parent Session 读取，config 中
/// 对应字段仅作 parent 缺失（/bg 命令等无 session 路径）时的回退。
#[allow(clippy::type_complexity)]
pub struct SubagentSpawnConfig {
    // ── 意图 ──
    /// 子 agent 名（事件 agent_name / thread title / task agent_name）
    pub agent_name: String,
    /// 派发给子 Agent 的任务描述（fork 路径经 fork directive 包装后入队）
    pub prompt: String,
    /// 父会话消息历史（fork 路径注入 transcript 让子 agent 理解上下文）
    pub parent_messages: Vec<BaseMessage>,
    /// 取消策略（Cascade = 父 cancel 传播，Independent = 独立）
    pub cancel_policy: SubagentCancelPolicy,
    /// 最大 ReAct 迭代次数
    pub max_iterations: usize,
    /// fork directive 模板（None = 不包装，直接 push prompt——agent 定义路径）
    pub fork_directive_kind: Option<ForkDirectiveKind>,
    /// 运行模式
    pub run_mode: SubagentRunMode,
    /// agent 定义声明的 skills（SkillPreload 装配输入）
    pub skill_names: Vec<String>,
    // ── 装配产物 ──
    /// SubAgent LLM（ReactLLM 实现/装饰器）
    pub llm: Box<dyn ReactLLM + Send + Sync>,
    /// 子 agent 中间件链装配器（middlewares 实现，链序契约 ARC-MIDDLEWARE-001）
    pub chain_assembler: Arc<dyn SubagentChainAssembler>,
    /// 过滤后的工具集（agent 定义路径按 tools/disallowed_tools 过滤）
    pub tools: Vec<Arc<dyn BaseTool>>,
    /// SubAgent system prompt（注入 transcript 起始处）
    pub system_prompt: Option<String>,
    /// 错误感知建议注册表（可选）
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（None 用 default）
    pub tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    /// deferred 工具解析器（None = DirectToolInvocationResolver；middlewares 传
    /// ExecuteExtraToolResolver 保持包装层语义）
    pub tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    /// auto-compact 阈值配置（None = 不启用）
    pub compact_config: Option<CompactConfig>,
    /// 上下文预算（None = 不追踪 token 使用率）
    pub context_budget: Option<ContextBudget>,
    /// Full Compact 专用 LLM（None 时 Full Compact 跳过）
    pub compact_llm: Option<Arc<dyn peri_model::Model>>,
    // ── 运行时通道 ──
    /// 线程持久化存储（None = 不落库，仅测试/遗留路径）
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 父 agent 事件 handler（同步路径事件转发 / 重试事件追踪）
    pub event_handler: Option<Arc<dyn AgentEventHandler>>,
    /// bg 任务完成事件发送通道（bg pump）
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// 后台任务管理器（Background 模式必填）
    pub task_manager: Option<Arc<TaskManager>>,
    /// bg 完成同步回调
    pub on_bg_complete:
        Option<Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    /// Langfuse bridge
    pub langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_start: Option<SubagentLifecycleStart>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_stop: Option<SubagentLifecycleStop>,
    /// 子 agent 启动注册回调
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// 子 agent 结束注销回调
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
    /// 父 agent 事件侧 AgentId（v2 SubagentStart/Stop 的 agent_id 字段；
    /// None = /bg 命令等无 Langfuse tracer 路径 → 不 emit v2 Start/Stop）
    pub parent_agent_id: Option<AgentId>,
    // ── 父侧数据回退（parent 为 None 时使用；parent 存在时被覆盖） ──
    /// 父 cancel token（Cascade 时取其 child_token；parent 存在时从 parent 读取）
    pub cancel_token: Option<CancellationToken>,
    /// 工作目录
    pub cwd: Option<String>,
    /// 父线程 ID
    pub parent_thread_id: Option<String>,
    /// Frozen CLAUDE.md main content（回退值）
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content（父 session 无此字段，恒由上层注入）
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary（回退值）
    pub frozen_skill_summary: Option<String>,
    /// Frozen 日期 YYYY-MM-DD（回退值）
    pub frozen_date: Option<String>,
}

/// spawn 产物
pub struct SubagentSpawned {
    /// 子线程 ID（= 子 session thread_id = 身份键来源）
    pub child_thread_id: String,
    /// 后台任务 ID（仅 Background 模式；格式 bg-{uuid v7}）
    pub task_id: Option<String>,
    /// 子 session（调用方读取 transcript）
    pub session: Arc<Session>,
    /// 生成的 cancel token（Background 注册 / 返回消息使用）
    pub cancel_token: CancellationToken,
    /// 是否被中断（Sync 模式；Background 模式恒 false）
    pub interrupted: bool,
}

// ─── 统一入口 ────────────────────────────────────────────────────────────────

/// Agent 层 session 工厂（L3）：subagent 创建统一入口命名空间。
///
/// 验收契约（子 issue L3）：`SessionFactory::spawn_subagent(parent, config)`
/// 为唯一 subagent 创建入口，位于 peri-agent。Middleware 只组装
/// [`SubagentSpawnConfig`] 发起意图，不持有创建实现。
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionFactory;

impl SessionFactory {
    /// 启动子 agent（唯一创建入口，见 [`spawn_subagent_impl`] 的流程说明）。
    pub async fn spawn_subagent(
        parent: Option<&Arc<Session>>,
        config: SubagentSpawnConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        spawn_subagent_impl(parent, config).await
    }
}

/// 启动子 agent（统一创建入口实现，L3）。
///
/// 流程（与迁移前四条路径语义一致）：
/// 1. 生成 child_thread_id / task_id
/// 2. 解析父侧数据（parent 优先；frozen copy 自 parent session，不重读磁盘）
/// 3. 创建子线程（thread_store Some 时；parent_thread_id 挂父子链）
/// 4. 构造子 session（frozen copy + transcript with_persistence 绑定存储）
/// 5. 注入 parent_messages / system_prompt 到 transcript，push prompt 到 queue
/// 6. 经 chain_assembler 装配子链（frozen 注入链上下文），构造 StageContext
/// 7. Sync：直接 run_react_loop；Background：tokio::spawn + TaskManager 注册
/// 8. 收尾：update_thread_status（done/cancelled/error）+ 事件 + hook 闭包
///
/// 并发限制（Background 最多 3 个活跃任务）：不做入口预检，由注册阶段的
/// `register_with_kind`（per-kind 上限）如实返回注册失败——与迁移前一致，
/// 预检（若有）位于调用方（llm_factory 之前），保证「预检 → 装配 → 注册」
/// 的确定性窗口不被重复预检破坏（S3.1 幽灵任务回归测试依赖此结构）。
#[allow(clippy::too_many_arguments)]
async fn spawn_subagent_impl(
    parent: Option<&Arc<Session>>,
    config: SubagentSpawnConfig,
) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
    // 解构 config：字段分散使用，避免部分 move 后整体借用冲突
    let SubagentSpawnConfig {
        agent_name,
        prompt,
        parent_messages,
        cancel_policy,
        max_iterations,
        fork_directive_kind,
        run_mode,
        skill_names,
        llm,
        chain_assembler,
        tools,
        system_prompt,
        error_suggest_registry,
        tool_registry_snapshot,
        tool_invocation_resolver,
        compact_config,
        context_budget,
        compact_llm,
        thread_store,
        event_handler,
        bg_event_sender,
        task_manager,
        on_bg_complete,
        langfuse_bridge,
        on_subagent_start,
        on_subagent_stop,
        register_runtime,
        deregister_runtime,
        parent_agent_id,
        cancel_token: cancel_token_cfg,
        cwd: cwd_cfg,
        parent_thread_id: parent_thread_id_cfg,
        frozen_claude_md: frozen_claude_md_cfg,
        frozen_claude_local_md: frozen_claude_local_md_cfg,
        frozen_skill_summary: frozen_skill_summary_cfg,
        frozen_date: frozen_date_cfg,
    } = config;

    // 并发限制由注册阶段兜底（register_with_kind per-kind 上限，错误如实返回），
    // 不在入口预检：middlewares 路径的预检位于 llm_factory 之前（execute_bg.rs），
    // 保证并发竞态窗口内错误语义与迁移前一致（"Failed to register"，S3.1）。

    // 2. 生成标识符
    let child_thread_id = uuid::Uuid::now_v7().to_string();
    let task_id = format!("bg-{}", uuid::Uuid::now_v7());

    // 3. 父侧数据解析（parent 优先；frozen data 从父 session copy）
    let cwd = parent
        .map(|p| p.store().cwd.to_string())
        .or(cwd_cfg)
        .ok_or("spawn_subagent: cwd 未提供（parent 缺失且 config.cwd 为 None）")?;
    let parent_thread_id = parent
        .and_then(|p| p.store().thread_id.clone())
        .or(parent_thread_id_cfg);
    let frozen_claude_md = parent
        .map(|p| p.store().frozen.claude_md.to_string())
        .or(frozen_claude_md_cfg);
    let frozen_skill_summary = parent
        .map(|p| p.store().frozen.skill_summary.to_string())
        .or(frozen_skill_summary_cfg);
    let frozen_date = parent
        .map(|p| p.store().frozen.date.to_string())
        .or(frozen_date_cfg);
    let frozen_claude_local_md = frozen_claude_local_md_cfg;

    // cancel token：Cascade = 父 cancel 传播（parent 优先，回退 config 注入的
    // 父 token；均缺失时新建），Independent = 新建（与迁移前语义一致）
    let cancel_token = match cancel_policy {
        SubagentCancelPolicy::Cascade => parent
            .map(|p| p.config().cancel_token.child_token())
            .or_else(|| cancel_token_cfg.map(|t| t.child_token()))
            .unwrap_or_default(),
        SubagentCancelPolicy::Independent => CancellationToken::new(),
    };
    let cancel_policy = cancel_policy.as_cancel_policy();

    // 4. 创建子线程（thread_store Some 时；None 跳过落库——仅测试/遗留路径）
    if let Some(ref store) = thread_store {
        let snapshot_id = parent_messages.last().map(|m| m.id().as_uuid().to_string());
        let mut child_meta = ThreadMeta::new(&cwd);
        child_meta.id = child_thread_id.clone();
        child_meta.parent_thread_id = parent_thread_id.clone();
        child_meta.snapshot_at_message_id = snapshot_id;
        child_meta.hidden = true;
        child_meta.cancel_policy = cancel_policy;
        child_meta.title = Some(agent_name.clone());
        store
            .create_thread(child_meta)
            .await
            .map_err(|e| format!("Failed to create child thread: {}", e))?;
    }

    // 5. 构造子 session：frozen 从父 copy（不重读磁盘），transcript 绑定存储
    let frozen = FrozenContext {
        system_prompt: parent
            .map(|p| Arc::clone(&p.store().frozen.system_prompt))
            .unwrap_or_default(),
        claude_md: frozen_claude_md
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        skill_summary: frozen_skill_summary
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        date: frozen_date
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        language: parent.and_then(|p| p.store().frozen.language.clone()),
    };
    let cancel_arc: Arc<CancellationToken> = Arc::new(cancel_token.clone());
    // SubAgent 独立 MessageQueue（不与 main agent 共享）
    let queue = MessageQueue::new();
    let session = Session::new_with_cancel_and_queue(
        Arc::from(cwd.as_str()),
        frozen,
        Some(child_thread_id.clone()),
        cancel_arc,
        queue,
    );

    // transcript 绑定持久化（subagent 必有持久化 thread；thread_id = agent_id）
    if let Some(ref store) = thread_store {
        let transcript_arc = session.transcript();
        let mut transcript = transcript_arc.write();
        let old = std::mem::take(&mut *transcript);
        *transcript = old.with_persistence(Arc::clone(store), child_thread_id.clone());
    }

    let transcript = session.transcript();

    // 6a. fork 路径：把 parent_messages 注入 transcript（让子 agent 看到父会话上下文）
    if !parent_messages.is_empty() {
        let mut tx = transcript.write();
        for msg in &parent_messages {
            tx.append(msg.clone());
        }
    }

    // 6b. SubAgent system_prompt（身份构建）注入到 transcript 开头位置：
    // - fork 路径：在 parent_messages 之后（让身份提示词位于对话上下文之后、
    //   prompt 之前——SubAgent 的 prompt 由下方 push 到 queue，Receive 阶段追加）
    // - 非 fork 路径：parent_messages 为空，直接 append 到 transcript 开头
    //
    // 注意：这是 session 起始身份构建（在 run_react_loop 调用前注入），不是中途纠正，
    // 用 BaseMessage::System 合法（CLAUDE.md TRAP 仅禁止中途纠正用 System）。
    if let Some(sp) = system_prompt {
        let mut tx = transcript.write();
        tx.append(BaseMessage::system(sp));
    }

    // 7. 子链装配（frozen 数据注入链上下文；链序由 assembler 实现方保持）
    let chain = chain_assembler.assemble(&SubagentChainContext {
        cwd: cwd.clone(),
        skill_names,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
    });

    // StageContext 构造（v2_bridge 迁移；tool_invocation_resolver 参数化；
    // 复用上面预创建的 session——transcript 已绑定持久化并注入父消息/system_prompt）
    let v2_ctx = build_v2_subagent_context(
        Some(session.clone()),
        llm,
        chain,
        tools,
        &cwd,
        cancel_token.clone(),
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        Some(agent_id_from_child_thread(&child_thread_id)),
    );

    // 6c. push prompt 到 queue（fork 路径套 fork directive 模板）
    let prompt_message = match fork_directive_kind {
        Some(ForkDirectiveKind::Fork) => build_fork_directive(&prompt),
        Some(ForkDirectiveKind::Bg) => build_bg_fork_directive(&prompt),
        None => prompt.clone(),
    };
    v2_ctx.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(prompt_message),
    ));

    match run_mode {
        SubagentRunMode::Sync => {
            let interrupted = run_sync_subagent(
                &child_thread_id,
                &agent_name,
                &cwd,
                max_iterations,
                event_handler,
                on_subagent_start,
                on_subagent_stop,
                thread_store,
                register_runtime,
                deregister_runtime,
                langfuse_bridge,
                parent_agent_id,
                v2_ctx,
                session.clone(),
            )
            .await?;
            Ok(SubagentSpawned {
                child_thread_id,
                task_id: None,
                session,
                cancel_token,
                interrupted,
            })
        }
        SubagentRunMode::Background => {
            let task_id_clone = task_id.clone();
            spawn_background_subagent(
                task_id.clone(),
                child_thread_id.clone(),
                agent_name.clone(),
                prompt,
                cwd.clone(),
                max_iterations,
                bg_event_sender,
                task_manager,
                on_bg_complete,
                langfuse_bridge,
                thread_store,
                deregister_runtime,
                on_subagent_start,
                on_subagent_stop,
                register_runtime,
                parent_agent_id,
                cancel_token.clone(),
                v2_ctx,
            )
            .await?;
            Ok(SubagentSpawned {
                child_thread_id,
                task_id: Some(task_id_clone),
                session,
                cancel_token,
                interrupted: false,
            })
        }
    }
}

// ─── 同步运行 ────────────────────────────────────────────────────────────────

/// 同步子 agent：当前调用内 run_react_loop，完成后收尾。
#[allow(clippy::too_many_arguments)]
async fn run_sync_subagent(
    child_thread_id: &str,
    agent_name: &str,
    cwd: &str,
    max_iterations: usize,
    event_handler: Option<Arc<dyn AgentEventHandler>>,
    on_subagent_start: Option<SubagentLifecycleStart>,
    on_subagent_stop: Option<SubagentLifecycleStop>,
    thread_store: Option<Arc<dyn ThreadStore>>,
    register_runtime: Option<RegisterRuntimeFn>,
    deregister_runtime: Option<DeregisterRuntimeFn>,
    langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    parent_agent_id: Option<AgentId>,
    v2_ctx: V2SubagentContext,
    session: Arc<Session>,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let agent_name = agent_name.to_string();
    let cwd = cwd.to_string();

    // 启动注册（active_agents，与 DeregisterGuard drop 配对）
    if let Some(register) = &register_runtime {
        register(
            child_thread_id.to_string(),
            (*session.config().cancel_token).clone(),
            "cascade".into(),
        );
    }
    let _deregister_guard = DeregisterGuard {
        thread_id: child_thread_id.to_string(),
        deregister: deregister_runtime,
    };

    // lifecycle hook（SubagentStart）
    if let Some(ref on_start) = on_subagent_start {
        on_start(&agent_name, &cwd);
    }

    // v2 SubagentStart（C2）：parent_agent_id 未注入时静默跳过（helper 内 warn）
    emit_subagent_start_v2(
        &v2_ctx.event_bus,
        v2_ctx.context.turn_id(),
        parent_agent_id,
        v2_ctx.agent_id,
        &agent_name,
        false,
    );
    // v1 协议化载体直发（SubagentStarted）：发射语义单一事实源为 v2 事件构造
    // （ObserveEvent 身份透传：child_agent_id → instance_id），经
    // `observe_event_to_executor` 同步映射后直发父 handler——同步保证 Started
    // 恒先于本 turn 后续事件到达父协议化链路（v1 ExecutorEvent 中间态已退役，
    // 仅保留 ACP 协议序列化面映射，`2026-07-18-executor-event-retirement.md`）。
    forward_subagent_start_v1(
        event_handler.as_ref(),
        build_subagent_start_v2(
            v2_ctx.context.turn_id(),
            parent_agent_id,
            v2_ctx.agent_id,
            &agent_name,
            false,
        ),
    );

    // v2 事件转发器：子 EventBus → 父事件 handler（TUI 可见子 agent 工具调用/AI 文本）
    let _forwarder_handle = spawn_subagent_event_forwarder(
        v2_ctx.event_handles,
        event_handler.clone(),
        langfuse_bridge,
        child_thread_id.to_string(),
    );

    // 运行 v2 ReAct 循环
    let subagent_turn_id = v2_ctx.context.turn_id();
    let loop_result = run_react_loop(v2_ctx.context, max_iterations).await;

    // v2 SubagentStop（C3）：一个 emit 点覆盖 Completed / Interrupted / Error 三路
    let (stop_result, stop_is_error) = match &loop_result {
        LoopResult::Completed => (
            extract_last_ai_text(&session)
                .chars()
                .take(500)
                .collect::<String>(),
            false,
        ),
        LoopResult::Interrupted => ("interrupted".to_string(), true),
        LoopResult::Error(e) => (
            format!("{} execution failed: {}", agent_name, e)
                .chars()
                .take(500)
                .collect::<String>(),
            true,
        ),
    };
    emit_subagent_stop_v2(
        &v2_ctx.event_bus,
        subagent_turn_id,
        parent_agent_id,
        v2_ctx.agent_id,
        &agent_name,
        &stop_result,
        stop_is_error,
    );
    // v1 协议化载体直发（SubagentStopped）：与 Started 同源（v2 事件构造 +
    // observe_event_to_executor 同步映射），保证 Stopped 在 turn 收尾前到达
    // 父协议化链路（TUI 容器销毁 / depth 配对）。
    forward_subagent_stop_v1(
        event_handler.as_ref(),
        build_subagent_stop_v2(
            subagent_turn_id,
            parent_agent_id,
            v2_ctx.agent_id,
            &agent_name,
            &stop_result,
            stop_is_error,
        ),
    );

    let (final_text, interrupted) = match loop_result {
        LoopResult::Completed => {
            let text = extract_last_ai_text(&session);
            (text, false)
        }
        LoopResult::Interrupted => (String::new(), true),
        LoopResult::Error(e) => {
            let error_summary = format!("{} execution failed: {}", agent_name, e);
            let error_result: String = error_summary.chars().take(500).collect();
            // 统一后处理（hook + thread_store；v1 协议化直发已在 emit_subagent_stop_v2
            // 之后经 forward_subagent_stop_v1 发出）
            on_subagent_stop_handler(
                &on_subagent_stop,
                &thread_store,
                &agent_name,
                child_thread_id,
                &error_result,
                true,
                &cwd,
            )
            .await;
            return Err(error_summary.into());
        }
    };

    let output_summary: String = if interrupted {
        "interrupted".to_string()
    } else {
        final_text.chars().take(500).collect()
    };
    on_subagent_stop_handler(
        &on_subagent_stop,
        &thread_store,
        &agent_name,
        child_thread_id,
        &output_summary,
        interrupted,
        &cwd,
    )
    .await;

    Ok(interrupted)
}

// ─── 后台运行 ────────────────────────────────────────────────────────────────

/// 后台子 agent：tokio::spawn 包装运行 + TaskManager 注册（S3.1 gate）+ 收尾。
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
async fn spawn_background_subagent(
    task_id: String,
    child_thread_id: String,
    agent_name: String,
    prompt: String,
    cwd: String,
    max_iterations: usize,
    bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    task_manager: Option<Arc<TaskManager>>,
    on_bg_complete: Option<
        Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>,
    >,
    langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
    thread_store: Option<Arc<dyn ThreadStore>>,
    deregister_runtime: Option<DeregisterRuntimeFn>,
    on_subagent_start: Option<SubagentLifecycleStart>,
    on_subagent_stop: Option<SubagentLifecycleStop>,
    register_runtime: Option<RegisterRuntimeFn>,
    parent_agent_id: Option<AgentId>,
    cancel_token: CancellationToken,
    v2_ctx: V2SubagentContext,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let task_manager =
        task_manager.ok_or("Background tasks not available: no task manager configured")?;
    let task_manager_spawn = Arc::clone(&task_manager);

    let prompt_summary: String = prompt.chars().take(100).collect();

    // S3.1 注册门控：spawn 包装任务，闭包第一步 await 注册结果 oneshot。
    let (reg_tx, reg_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

    let task_id_for_task = task_id.clone();
    let child_thread_id_for_task = child_thread_id.clone();
    let agent_name_for_task = agent_name.clone();
    let prompt_summary_for_task = prompt_summary.clone();
    let cwd_for_task = cwd.clone();

    let join_handle = tokio::spawn(async move {
        // S3.1 门控：注册结果（失败时调用方已发 Err；sender 被 drop 同样返回）
        match reg_rx.await {
            Ok(Ok(())) => {}
            _ => return,
        }

        let started_at = std::time::Instant::now();
        // context 将被 move 进 run_react_loop，turn_id 提前提取（Start/Stop emit 用）
        let subagent_turn_id = v2_ctx.context.turn_id();
        let context = v2_ctx.context;
        let session = v2_ctx.session;
        // Start/Stop emit 需要 event_bus（partial move 后仍可用）+ 统一身份键
        let event_bus_for_emit = v2_ctx.event_bus.clone();
        let subagent_agent_id = v2_ctx.agent_id;

        // S3.2 同步收尾 guard：abort/panic 时 deregister_runtime + 补发
        // v2 SubagentStop（含 v1 协议化直发，与 SubagentStarted 配对）。
        // 必须在本段事件 emit 之前构造。
        let mut cleanup_guard = BgCleanupGuard {
            thread_id: child_thread_id_for_task.clone(),
            deregister: deregister_runtime.clone(),
            stop: Some(BgStopEmitV2 {
                event_bus: event_bus_for_emit.clone(),
                turn_id: subagent_turn_id,
                parent_agent_id,
                child_agent_id: subagent_agent_id,
                agent_name: agent_name_for_task.clone(),
                // v1 协议化直发目标（bg 泵；None = 无 bg 通道，仅 v2 补发）
                sender: bg_event_sender.clone(),
            }),
        };

        // v1 协议化发射目标（bg 泵）：BG pump 独立于主 pump，主 turn 结束后仍存活。
        // 构造提前到 Started 直发之前（start 借用、stop 直发 clone、forwarder move）。
        let bg_forwarder_handler: Option<Arc<dyn AgentEventHandler>> =
            bg_event_sender.clone().map(|tx| {
                Arc::new(crate::agent::events::FnEventHandler(
                    move |ev: ExecutorEvent| {
                        let _ = tx.send(ev);
                    },
                )) as Arc<dyn AgentEventHandler>
            });
        let bg_stop_handler = bg_forwarder_handler.clone();

        // lifecycle hook（SubagentStart）
        if let Some(ref on_start) = on_subagent_start {
            on_start(&agent_name_for_task, &cwd_for_task);
        }

        // v2 SubagentStart（C2）：与 lifecycle hook 同点、同通道（child EventBus）。
        emit_subagent_start_v2(
            &event_bus_for_emit,
            subagent_turn_id,
            parent_agent_id,
            subagent_agent_id,
            &agent_name_for_task,
            true,
        );
        // v1 协议化载体直发（SubagentStarted）：发射语义单一事实源为 v2 事件构造
        // （ObserveEvent 身份透传：child_agent_id → instance_id），经
        // `observe_event_to_executor` 同步映射后直发 bg_event_sender——同步保证
        // Started 恒先于任何 SubagentStopped / BackgroundTaskCompleted
        // （正常/取消/abort 三路，P2 顺序契约）。
        if bg_event_sender.is_some() {
            forward_subagent_start_v1(
                bg_forwarder_handler.as_ref(),
                build_subagent_start_v2(
                    subagent_turn_id,
                    parent_agent_id,
                    subagent_agent_id,
                    &agent_name_for_task,
                    true,
                ),
            );
        } else {
            tracing::warn!(
                agent = %agent_name_for_task,
                instance_id = %child_thread_id_for_task,
                "bg_event_sender unavailable, SubagentStarted event dropped"
            );
        }

        // 启动 v2 事件转发器：消费 SubAgent EventBus 的事件，注入 source_agent_id
        // 后转发到 bg_event_sender（BG pump 独立于主 pump，主 turn 结束后仍存活）。
        // SubagentStart/Stop 不在此转发（发射侧已同步协议化直发，防双发——
        // 见 `forward_subagent_start_v1` / `forward_subagent_stop_v1`）。
        let _forwarder_handle = spawn_subagent_event_forwarder(
            v2_ctx.event_handles,
            bg_forwarder_handler,
            langfuse_bridge,
            child_thread_id_for_task.clone(),
        );

        let loop_result = run_react_loop(context, max_iterations).await;

        // 补发 v2 SubagentStop（C3）：一个 emit 点覆盖 Completed / Interrupted / Error。
        let (stop_result, stop_is_error) = match &loop_result {
            LoopResult::Completed => (
                extract_last_ai_text(&session)
                    .chars()
                    .take(500)
                    .collect::<String>(),
                false,
            ),
            LoopResult::Interrupted => ("interrupted".to_string(), true),
            LoopResult::Error(e) => (
                format!("Background sub-agent failed: {}", e)
                    .chars()
                    .take(500)
                    .collect::<String>(),
                true,
            ),
        };
        emit_subagent_stop_v2(
            &event_bus_for_emit,
            subagent_turn_id,
            parent_agent_id,
            subagent_agent_id,
            &agent_name_for_task,
            &stop_result,
            stop_is_error,
        );
        // v1 协议化直发（SubagentStopped）在下方各分支显式执行（Error 分支 / 正常
        // 分支），此处仅闭合 v2 发射：guard drop 时不得重复（P1 防双发）。
        cleanup_guard.disarm_stop();

        let (final_text, interrupted) = match loop_result {
            LoopResult::Completed => (extract_last_ai_text(&session), false),
            LoopResult::Interrupted => (String::new(), true),
            LoopResult::Error(e) => {
                let output = format!("Background sub-agent failed: {}", e);
                // 错误路径：lifecycle hook + thread_store + registry notification
                if let Some(ref on_stop) = on_subagent_stop {
                    on_stop(&agent_name_for_task, &cwd_for_task, &output, true);
                }
                if let Some(ref store) = thread_store {
                    let _ = store
                        .update_thread_status(&child_thread_id_for_task, "error")
                        .await;
                }
                // 错误分支也必须发射 SubagentStopped（is_error=true），保证 depth 配对减 1。
                // v1 协议化直发从 v2 事件构造同步映射（发射语义单一事实源 = v2；
                // ObserveEvent 身份透传：child_agent_id → instance_id）。
                // 必须在 BackgroundTaskResult 构造之前发射——后者会 move output。
                forward_subagent_stop_v1(
                    bg_stop_handler.as_ref(),
                    build_subagent_stop_v2(
                        subagent_turn_id,
                        parent_agent_id,
                        subagent_agent_id,
                        &agent_name_for_task,
                        &output,
                        true,
                    ),
                );
                let result = crate::agent::events::BackgroundTaskResult {
                    task_id: task_id_for_task.clone(),
                    agent_name: agent_name_for_task.clone(),
                    prompt_summary: prompt_summary_for_task.clone(),
                    success: false,
                    output,
                    tool_calls_count: count_tool_calls_from_session(&session),
                    duration_ms: started_at.elapsed().as_millis() as u64,
                    child_thread_id: Some(child_thread_id_for_task.clone()),
                    timed_out: false,
                };
                // 同步推送 Defer 到 MQ——必须在 registry.complete() 之前
                if let Some(ref on_complete) = on_bg_complete {
                    on_complete(&result, BgTaskKind::Agent);
                }
                task_manager_spawn.complete(&task_id_for_task, result);
                // deregister 由 cleanup_guard drop 统一执行（正常/abort/panic 三路）
                return;
            }
        };

        let output_summary: String = if interrupted {
            "interrupted".to_string()
        } else {
            final_text.chars().take(500).collect()
        };

        // SubagentStopped v1 协议化直发 + lifecycle hook（经 bg_event_sender，
        // 与 spawner 对齐）。v1 从 v2 事件构造同步映射，保证 Stopped 先于
        // BackgroundTaskCompleted 到达 bg 泵（顺序契约）。
        forward_subagent_stop_v1(
            bg_stop_handler.as_ref(),
            build_subagent_stop_v2(
                subagent_turn_id,
                parent_agent_id,
                subagent_agent_id,
                &agent_name_for_task,
                &output_summary,
                interrupted,
            ),
        );
        if let Some(ref on_stop) = on_subagent_stop {
            on_stop(
                &agent_name_for_task,
                &cwd_for_task,
                &output_summary,
                interrupted,
            );
        }

        // thread_store 状态
        if let Some(ref store) = thread_store {
            let status = if interrupted { "cancelled" } else { "done" };
            let _ = store
                .update_thread_status(&child_thread_id_for_task, status)
                .await;
        }

        // 后台任务完成通知（注入到主 agent 消息流）
        let result = crate::agent::events::BackgroundTaskResult {
            task_id: task_id_for_task.clone(),
            agent_name: agent_name_for_task.clone(),
            prompt_summary: prompt_summary_for_task.clone(),
            success: !interrupted,
            output: if interrupted {
                "Background sub-agent was interrupted".to_string()
            } else {
                final_text
            },
            tool_calls_count: count_tool_calls_from_session(&session),
            duration_ms: started_at.elapsed().as_millis() as u64,
            child_thread_id: Some(child_thread_id_for_task.clone()),
            timed_out: false,
        };
        if let Some(ref sender) = bg_event_sender {
            let _ = sender.send(ExecutorEvent::BackgroundTaskCompleted(result.clone()));
        } else {
            tracing::warn!(
                task_id = %task_id_for_task,
                "bg_event_sender unavailable, BackgroundTaskCompleted event dropped"
            );
        }
        // 同步推送 Defer 到 MQ——必须在 registry.complete() 之前
        // 确保 active_count 归零时 Defer 已在 MQ 中
        if let Some(ref on_complete) = on_bg_complete {
            on_complete(&result, BgTaskKind::Agent);
        }
        task_manager_spawn.complete(&task_id_for_task, result);
        // deregister 由 cleanup_guard drop 统一执行（正常/abort/panic 三路）
    });

    // 注册到 BackgroundTaskRegistry
    let bg_task = BackgroundTask {
        id: task_id.clone(),
        agent_name: agent_name.clone(),
        prompt_summary,
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(join_handle),
        cancel_token: Some(cancel_token.clone()),
        pid: None,
        output_preview: None,
    };
    if let Err(e) = task_manager.register_with_kind(bg_task) {
        // S3.1：注册失败（并发撞 kind 上限）——通知包装任务直接 return（不执行
        // run_react_loop、不 emit 任何事件），再如实返回错误。任务零事件零注册，
        // 无幽灵执行 / 无泄漏。
        let _ = reg_tx.send(Err(e.to_string()));
        return Err(format!("Failed to register background task: {}", e).into());
    }
    // 注册成功：先注册运行时（active_agents，与任务内 guard 的 deregister 配对），
    // 再放行包装任务继续执行。
    if let Some(register) = &register_runtime {
        register(child_thread_id.clone(), cancel_token, "independent".into());
    }
    let _ = reg_tx.send(Ok(()));

    Ok(())
}

// ─── v2 桥接（自 peri-middlewares/src/subagent/v2_bridge.rs 迁移） ──────────

/// SubAgent v2 上下文产物
pub struct V2SubagentContext {
    /// v2 StageContext（传给 run_react_loop）
    pub context: StageContext,
    /// v2 Session（调用方持有以读取 transcript）
    pub session: Arc<Session>,
    /// EventBus 消费端（调用方 spawn forwarder 用）
    pub event_handles: EventHandles,
    /// 统一后的 subagent 身份键（= child_thread_id 的 AgentId 形式）
    pub agent_id: AgentId,
    /// EventBus 生产端（补发 SubagentStart/Stop 等 ObserveEvent 用）
    pub event_bus: Arc<EventBus>,
}

/// 从 `child_thread_id`（UUID v7 字符串）解析统一身份键 `AgentId`（C1）。
///
/// 身份契约：`child_thread_id`、subagent session `AgentId`、`instance_id`、
/// forwarder `source_agent_id`、事件 `agent_id` 收敛为同一 UUID。
pub fn agent_id_from_child_thread(child_thread_id: &str) -> AgentId {
    AgentId::from_uuid(
        uuid::Uuid::parse_str(child_thread_id)
            .expect("child_thread_id 由 Uuid::now_v7() 生成，必为合法 UUID"),
    )
}

/// 构造 v2 `SubagentStart` 事件（发射语义单一事实源）。
///
/// `agent_id` 为父视角归属身份：`parent_agent_id` 未注入（/bg、测试路径）时以
/// `child_agent_id` 占位——v1 协议化映射（`observe_event_to_executor`）不消费
/// 该字段，仅 v2 emit（Langfuse tracer 归属）需要真实父身份。
pub(crate) fn build_subagent_start_v2(
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    is_background: bool,
) -> ObserveEvent {
    ObserveEvent::SubagentStart {
        turn_id,
        agent_id: parent_agent_id.unwrap_or(child_agent_id),
        child_agent_id,
        agent_name: agent_name.to_string(),
        is_background,
    }
}

/// 经 child EventBus 发射 v2 `SubagentStart`（C2）。
///
/// `parent_agent_id` 为 None（未注入/测试路径）时不 emit，仅 tracing warn——
/// 防脏数据：缺父身份的事件会让 tracer 无法归属，宁可走 incomplete 分支。
/// （v1 协议化直发不依赖本函数：`forward_subagent_start_v1` 独立于父身份。）
pub(crate) fn emit_subagent_start_v2(
    event_bus: &Arc<EventBus>,
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    is_background: bool,
) {
    if parent_agent_id.is_none() {
        tracing::warn!(
            target: "langfuse::subagent",
            child_agent_id = %child_agent_id,
            agent_name,
            "parent_agent_id 未注入，跳过 v2 SubagentStart emit（防脏数据）"
        );
        return;
    }
    event_bus.emit_observe(build_subagent_start_v2(
        turn_id,
        parent_agent_id,
        child_agent_id,
        agent_name,
        is_background,
    ));
}

/// 构造 v2 `SubagentStop` 事件（发射语义单一事实源）。占位规则同
/// [`build_subagent_start_v2`]。
pub(crate) fn build_subagent_stop_v2(
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    result: &str,
    is_error: bool,
) -> ObserveEvent {
    ObserveEvent::SubagentStop {
        turn_id,
        agent_id: parent_agent_id.unwrap_or(child_agent_id),
        child_agent_id,
        agent_name: agent_name.to_string(),
        result: result.to_string(),
        is_error,
    }
}

/// 经 child EventBus 发射 v2 `SubagentStop`（C3）。
///
/// 与 [`emit_subagent_start_v2`] 同一通道；parent_agent_id 为 None 时同样跳过。
pub(crate) fn emit_subagent_stop_v2(
    event_bus: &Arc<EventBus>,
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    result: &str,
    is_error: bool,
) {
    if parent_agent_id.is_none() {
        tracing::warn!(
            target: "langfuse::subagent",
            child_agent_id = %child_agent_id,
            agent_name,
            "parent_agent_id 未注入，跳过 v2 SubagentStop emit（防脏数据）"
        );
        return;
    }
    event_bus.emit_observe(build_subagent_stop_v2(
        turn_id,
        parent_agent_id,
        child_agent_id,
        agent_name,
        result,
        is_error,
    ));
}

// ─── v1 协议化载体直发（发射侧同步映射） ───────────────────────────────────
//
// v1 `ExecutorEvent` 中间态已退役（`2026-07-18-executor-event-retirement.md`）：
// SubagentStart/Stop 的发射语义单一事实源为 v2 事件构造，v1 仅作 ACP 协议化
// 载体——经 `peri-acp-types::event_v2::observe_event_to_executor`（协议序列化面
// 保留的最小映射）同步映射后直发父 handler / bg 泵。同步直发（非 forwarder
// 异步转发）保证 Started/Stopped 与 BackgroundTaskCompleted 的顺序契约；
// 转发器（`subagent_event_forwarder`）对 v2 SubagentStart/Stop 保持过滤（防双发）。

/// v1 协议化直发 `SubagentStarted`（从 v2 事件同步映射）。
///
/// `handler` 为 None（无父 handler / 无 bg 通道）时静默跳过。
fn forward_subagent_start_v1(handler: Option<&Arc<dyn AgentEventHandler>>, ev: ObserveEvent) {
    let Some(h) = handler else { return };
    // SubagentStarted 无 source_agent_id 字段（TUI 按 instance_id 配对），
    // 无需 set_source_agent_id；instance_id 由 child_agent_id 身份透传（C1）。
    if let Some(exec_ev) = observe_event_to_executor(ev) {
        h.on_event(exec_ev);
    }
}

/// v1 协议化直发 `SubagentStopped`（从 v2 事件同步映射）。语义同
/// [`forward_subagent_start_v1`]。
fn forward_subagent_stop_v1(handler: Option<&Arc<dyn AgentEventHandler>>, ev: ObserveEvent) {
    let Some(h) = handler else { return };
    if let Some(exec_ev) = observe_event_to_executor(ev) {
        h.on_event(exec_ev);
    }
}

/// 构造 SubAgent v2 上下文（自 `build_v2_subagent_context` 迁移；
/// `tool_invocation_resolver` 参数化避免 Agent 层反向依赖 middlewares）。
///
/// `session` 为调用方预创建的子 session（transcript 已绑定持久化、已注入
/// parent_messages / system_prompt）；None 时内部自建（测试/工具直调路径兜底，
/// 无持久化）。
#[allow(clippy::too_many_arguments)]
pub fn build_v2_subagent_context(
    session: Option<Arc<Session>>,
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain: MiddlewareChain,
    tools: Vec<Arc<dyn BaseTool>>,
    cwd: &str,
    cancel_token: CancellationToken,
    tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    compact_config: Option<CompactConfig>,
    context_budget: Option<ContextBudget>,
    compact_llm: Option<Arc<dyn peri_model::Model>>,
    agent_id: Option<AgentId>,
) -> V2SubagentContext {
    let session = match session {
        Some(s) => s,
        None => {
            let cwd_arc: Arc<str> = Arc::from(cwd);
            let frozen = FrozenContext::builder().build();
            let cancel_arc = Arc::new(cancel_token);
            // 自建兜底：独立 MessageQueue，无持久化
            let queue = MessageQueue::new();
            Session::new_with_cancel_and_queue(cwd_arc, frozen, None, cancel_arc, queue)
        }
    };

    let turn = session.start_turn();
    let transcript = session.transcript();
    let queue_clone = session.queue().clone();

    // tools → SharedToolMap（本地 tools 全部进 map）
    let mut tools_map: std::collections::BTreeMap<String, Arc<dyn BaseTool>> =
        std::collections::BTreeMap::new();
    for tool in tools {
        tools_map.insert(tool.name().to_string(), tool);
    }
    let combined_shared_tools: SharedToolMap = Arc::new(RwLock::new(tools_map));

    let (event_bus, event_handles) = EventBus::new(EventBusConfig::default());
    let event_bus_arc: Arc<EventBus> = Arc::new(event_bus);

    // 身份键统一（C1）：child_thread_id → AgentId；None（测试路径）内部生成。
    let resolved_agent_id = agent_id.unwrap_or_default();

    let session_context = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let v2_llm: Arc<dyn ReactLLM + Send + Sync> = Arc::from(llm);

    let snapshot = tool_registry_snapshot.unwrap_or_default();

    let mut builder = StageContext::builder(turn, transcript, queue_clone)
        .with_agent_id(resolved_agent_id)
        .with_llm(v2_llm)
        .with_tools(combined_shared_tools)
        .with_tool_invocation_resolver(tool_invocation_resolver.unwrap_or_else(|| {
            Arc::new(DirectToolInvocationResolver) as Arc<dyn ToolInvocationResolver>
        }))
        .with_middleware_chain(Arc::new(chain))
        .with_event_bus(Arc::clone(&event_bus_arc))
        .with_session_context(session_context)
        .with_tool_registry_snapshot(snapshot);

    if let Some(reg) = error_suggest_registry {
        builder = builder.with_error_suggest_registry(reg);
    }
    if let Some(budget) = context_budget {
        builder = builder.with_context_budget(budget);
    }
    if let Some(cc) = compact_config {
        builder = builder.with_compact_config(cc);
    }
    if let Some(llm) = compact_llm {
        builder = builder.with_compact_llm(llm);
    }
    // system_prompt 由 spawn_subagent 以 BaseMessage::System 注入 transcript
    //（StageContext.system_prompt 为死字段，不写入）。

    let context = builder.build();

    V2SubagentContext {
        context,
        session,
        event_handles,
        agent_id: resolved_agent_id,
        event_bus: event_bus_arc,
    }
}

/// SubAgent v2 上下文构建器（3.0 批 2 注入面）。
///
/// `build_v2_subagent_context` 的 trait 封装：ACP workflow agent 等协议面
/// 经装配注入本 trait 调用（不直接引用本层实现），默认实现即委托
/// [`build_v2_subagent_context`]（[`DefaultSubagentV2ContextBuilder`]）。
#[allow(clippy::too_many_arguments)]
pub trait SubagentV2ContextBuilder: Send + Sync {
    /// 构造 SubAgent v2 上下文（参数与 [`build_v2_subagent_context`] 一致）。
    fn build(
        &self,
        session: Option<Arc<Session>>,
        llm: Box<dyn ReactLLM + Send + Sync>,
        chain: MiddlewareChain,
        tools: Vec<Arc<dyn BaseTool>>,
        cwd: &str,
        cancel_token: CancellationToken,
        tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
        error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
        tool_registry_snapshot: Option<ToolRegistrySnapshot>,
        compact_config: Option<CompactConfig>,
        context_budget: Option<ContextBudget>,
        compact_llm: Option<Arc<dyn peri_model::Model>>,
        agent_id: Option<AgentId>,
    ) -> V2SubagentContext;
}

/// [`SubagentV2ContextBuilder`] 的默认实现：委托 [`build_v2_subagent_context`]。
pub struct DefaultSubagentV2ContextBuilder;

#[allow(clippy::too_many_arguments)]
impl SubagentV2ContextBuilder for DefaultSubagentV2ContextBuilder {
    fn build(
        &self,
        session: Option<Arc<Session>>,
        llm: Box<dyn ReactLLM + Send + Sync>,
        chain: MiddlewareChain,
        tools: Vec<Arc<dyn BaseTool>>,
        cwd: &str,
        cancel_token: CancellationToken,
        tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
        error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
        tool_registry_snapshot: Option<ToolRegistrySnapshot>,
        compact_config: Option<CompactConfig>,
        context_budget: Option<ContextBudget>,
        compact_llm: Option<Arc<dyn peri_model::Model>>,
        agent_id: Option<AgentId>,
    ) -> V2SubagentContext {
        build_v2_subagent_context(
            session,
            llm,
            chain,
            tools,
            cwd,
            cancel_token,
            tool_invocation_resolver,
            error_suggest_registry,
            tool_registry_snapshot,
            compact_config,
            context_budget,
            compact_llm,
            agent_id,
        )
    }
}

// ─── 生命周期工具（自 tool/lifecycle.rs 迁移；hook 触发闭包化） ────────────

/// RAII guard that calls deregister on drop (panic-safe cleanup).
pub(crate) struct DeregisterGuard {
    pub(crate) thread_id: String,
    pub(crate) deregister: Option<DeregisterRuntimeFn>,
}

impl Drop for DeregisterGuard {
    fn drop(&mut self) {
        if let Some(ref deregister) = self.deregister {
            deregister(&self.thread_id);
        }
    }
}

/// v2 SubagentStop 补发参数（BgCleanupGuard 取消兜底路径使用）。
///
/// 字段与 [`build_subagent_stop_v2`] 参数一一对应（C3 配对契约）：
/// abort 兜底路径下 v2 Start 已 emit 而 v2 Stop 永不 emit → Langfuse AGENT span
/// 悬挂，Drop 时经 child EventBus 补发；同时 v1 协议化直发（`sender` 存在时）
/// 补发 SubagentStopped——两者共用同一 v2 事件构造（发射语义单一事实源）。
pub(crate) struct BgStopEmitV2 {
    pub(crate) event_bus: Arc<EventBus>,
    pub(crate) turn_id: TurnId,
    pub(crate) parent_agent_id: Option<AgentId>,
    pub(crate) child_agent_id: AgentId,
    pub(crate) agent_name: String,
    /// v1 协议化直发目标（bg 泵；None = 无 bg 通道，仅 v2 补发）
    pub(crate) sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
}

/// bg 任务同步收尾 guard（S3.2）：Drop 时（任务被 abort / panic / 正常结束）执行：
/// - `deregister_runtime`（active_agents 清理，防泄漏）
/// - 补发 v2 `SubagentStop`（若未显式 emit——正常路径 emit 后需 `disarm_stop`）
///   + v1 协议化直发 `SubagentStopped`（sender 存在时，同一事件构造）
pub(crate) struct BgCleanupGuard {
    pub(crate) thread_id: String,
    pub(crate) deregister: Option<DeregisterRuntimeFn>,
    /// 未显式 emit v2 SubagentStop 时补发（取消/abort 兜底路径）
    pub(crate) stop: Option<BgStopEmitV2>,
}

impl BgCleanupGuard {
    /// 正常路径已显式 emit v2 SubagentStop + v1 协议化直发后调用，
    /// 防止 drop 时重复发射。
    pub(crate) fn disarm_stop(&mut self) {
        self.stop = None;
    }
}

impl Drop for BgCleanupGuard {
    fn drop(&mut self) {
        if let Some(ref deregister) = self.deregister {
            deregister(&self.thread_id);
        }
        if let Some(stop) = &self.stop {
            // 单一 v2 事件构造：v2 发射（parent 身份存在时）+ v1 协议化直发
            // （sender 存在时）。ObserveEvent 身份透传：child_agent_id → instance_id。
            let ev = build_subagent_stop_v2(
                stop.turn_id,
                stop.parent_agent_id,
                stop.child_agent_id,
                &stop.agent_name,
                "Background sub-agent was cancelled",
                true,
            );
            if stop.parent_agent_id.is_some() {
                stop.event_bus.emit_observe(ev.clone());
            }
            if let Some(sender) = &stop.sender {
                if let Some(exec_ev) = observe_event_to_executor(ev) {
                    let _ = sender.send(exec_ev);
                }
            }
        }
    }
}

/// 同步 SubAgent 停止统一后处理（fork + agent 定义路径）。
///
/// 按顺序执行：
/// 1. lifecycle hook (SubagentStop，经闭包)
/// 2. thread_store 状态更新（仅 sync 路径有此步骤）
///
/// v1 SubagentStopped 协议化直发不在本函数内——由调用方在
/// `emit_subagent_stop_v2` 之后经 `forward_subagent_stop_v1` 同步映射发出
/// （发射语义单一事实源 = v2 事件构造，v1 仅 ACP 协议化载体）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn on_subagent_stop_handler(
    on_subagent_stop: &Option<SubagentLifecycleStop>,
    thread_store: &Option<Arc<dyn ThreadStore>>,
    agent_id: &str,
    child_thread_id: &str,
    output_summary: &str,
    is_error: bool,
    cwd: &str,
) {
    // 1. lifecycle hook（闭包由 middlewares 构造，内部触发 RegisteredHook）
    if let Some(ref on_stop) = on_subagent_stop {
        on_stop(agent_id, cwd, output_summary, is_error);
    }
    // 3. thread_store（仅 sync 路径有此步骤）
    if let Some(ref store) = thread_store {
        let status = if is_error { "error" } else { "done" };
        let _ = store
            .update_thread_status(&child_thread_id.to_string(), status)
            .await;
    }
}

// ─── 工具函数（自 tool/mod.rs / mod.rs 迁移） ──────────────────────────────

/// 从 session transcript 提取最后一条非空 AI 消息文本（P1-11: 各执行路径共用）。
pub fn extract_last_ai_text(session: &Arc<Session>) -> String {
    let transcript = session.transcript();
    let tx = transcript.read();
    tx.visible_messages()
        .iter()
        .rev()
        .find_map(|m| {
            if matches!(m, BaseMessage::Ai { .. }) {
                let t = m.content();
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// 从 session transcript 统计 subagent 实际执行的工具调用次数。
///
/// 遍历 `visible_messages()` 中所有 `BaseMessage::Tool` 条目——每条对应一次
/// 工具执行（含成功和失败）。
pub fn count_tool_calls_from_session(session: &Arc<Session>) -> usize {
    let transcript = session.transcript();
    let tx = transcript.read();
    tx.visible_messages()
        .iter()
        .filter(|m| matches!(m, BaseMessage::Tool { .. }))
        .count()
}

/// Format sub-agent execution result as a summary string returned to the parent agent.
pub fn format_subagent_result(output: &AgentOutput) -> String {
    if output.tool_calls.is_empty() {
        return output.text.clone();
    }

    let mut tool_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (call, _) in &output.tool_calls {
        *tool_counts.entry(call.name.as_str()).or_insert(0) += 1;
    }

    let mut tools: Vec<_> = tool_counts.into_iter().collect();
    tools.sort_by_key(|b| std::cmp::Reverse(b.1));

    let tool_summary = tools
        .into_iter()
        .map(|(name, count)| format!("{} {} times", name, count))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "[Sub-agent executed {} tool calls: {}]\n\n{}",
        output.tool_calls.len(),
        tool_summary,
        output.text
    )
}

// ─── Fork 指令模板（自 fork.rs 迁移，纯字符串函数） ────────────────────────

/// Build fork directive message for fork mode.
pub fn build_fork_directive(prompt: &str) -> String {
    format!(
        "<fork_directive>\n\
         You are a forked agent continuing from the parent conversation.\n\
         You have full access to the conversation history above.\n\
         \n\
         RULES:\n\
         1. Do NOT spawn sub-agents — execute directly using your tools\n\
         2. Do NOT ask questions — act on the directive below\n\
         3. Stay strictly within your assigned scope\n\
         4. Report structured facts, then stop\n\
         5. Keep your response under 500 words unless specified otherwise\n\
         \n\
         Output format:\n\
           Scope: <your assigned scope in one sentence>\n\
           Result: <the answer or key findings>\n\
           Key files: <relevant file paths>\n\
           Files changed: <list if you modified files>\n\
         </fork_directive>\n\n\
         {prompt}"
    )
}

/// Build bg-fork directive message for /bg command path.
pub fn build_bg_fork_directive(prompt: &str) -> String {
    // 防御性 XML 注入防护
    let sanitized = prompt.replace("</bg_fork_directive>", "<\u{200b}/bg_fork_directive>");
    format!(
        "<bg_fork_directive>\n\
         你是后台异步 Agent，从父会话 fork 而来。\n\
         你拥有完整的对话历史上下文。\n\
         \n\
         规则：\n\
         1. 禁止生成子 Agent — 直接使用工具执行\n\
         2. 禁止提问 — 按指令行动\n\
         3. 严格限定在分配范围内\n\
         4. 先给出结论，再补充说明\n\
         5. 除非特别说明，回复控制在 500 字以内\n\
         \n\
         输出格式：\n\
           结论: <核心结论或答案>\n\
           详细说明: <补充细节>\n\
           关键文件: <相关文件路径>\n\
           建议: <后续行动建议>\n\
         </bg_fork_directive>\n\n\
         {sanitized}"
    )
}

/// 构建 Prediction 指令模板（中文）。
/// 用于 agent 完成后预测用户下一步输入。
///
/// `current_title` 为会话当前标题（`None` 表示尚无标题）。注入后模型才能判断
/// 现有标题是否需要更新——不传则模型无从得知标题现状，会默认不输出 title 标记。
pub fn build_prediction_directive(current_title: Option<&str>) -> String {
    // 防御性 XML 注入防护（标题可能含闭合标签文本）
    let title_ctx = match current_title {
        Some(t) => {
            let sanitized = t.replace("</prediction_directive>", "<\u{200b}/prediction_directive>");
            format!("当前会话标题：\"{sanitized}\"")
        }
        None => "当前会话标题：（无）".to_string(),
    };
    format!(
        "<prediction_directive>\n\
         你是预测输入助手。根据对话上下文，预测用户下一步最可能在输入框中输入什么，\n\
         并同步维护会话元数据。\n\
         \n\
         {title_ctx}\n\
         \n\
         规则：\n\
         1. 默认输出一句预测文本（占位符），不要解释\n\
         2. 预测应该是自然的用户语言，像用户自己会打的那样\n\
         3. 不要加引号、前缀或格式\n\
         4. 长度控制在 5-30 个字\n\
         5. 如果无法判断，输出空字符串\n\
         \n\
         结构化标记（仅在对应信息有价值时输出，可同时输出多个）：\n\
         - <peri:title>新标题</peri:title>：当标题缺失、过时或与当前任务不符时，主动更新为精炼的当前任务标题；话题转变时应立即更新\n\
         - <peri:tag>标签</peri:tag>：检测到明确主题时打一个标签（如 bugfix、refactor）\n\
         - <peri:summary>一句话摘要</peri:summary>：给整个对话写一句简短摘要\n\
         示例：继续排查内存泄漏 <peri:title>排查内存泄漏</peri:title><peri:tag>bugfix</peri:tag>\n\
         示例（话题转变，标题应立即更新）：<peri:title>性能优化</peri:title>\n\
         </prediction_directive>"
    )
}

#[cfg(test)]
#[path = "subagent_test.rs"]
mod tests;
