//! Session lifecycle management.
//!
//! Manages ACP session creation, loading, resumption, and closure.
//! Each session owns a ThreadStore entry, an Agent instance, and associated state.
//!
//! v2 迁移：AcpSession 瘦身为外部句柄，核心状态委托给
//! `peri_agent::session::Session`。保留 ACP 特有字段（provider_id、
//! model_alias、thinking、active_agents、goal_state）。
//!
//! L5（executor 拆分）：active_agents 注册表的条目类型与 cancel 判定/终止
//! 执行归 Agent 层（Cascade/Independent 判定经契约面
//! `peri_acp_types::session::cancel_cascade_agents` / `cancel_all_agents`），
//! 本层仅定位（查 session 映射）并传递注册表；注册表字段随 L2/L5 运行态
//! 归位迁入 `peri_agent::session::Session`。
//!
//! cron 主路径：session 级 CronOwner 由 `AcpSession.cron_bridge` 持有
//! （跨 turn 存活）；`set_async_owners` 仅 print fallback 使用。

pub mod agent_pool;
pub mod command;
pub mod cron_bridge;
pub mod event_sink;
pub mod executor;
pub mod goal_state;
pub mod retry_events;
pub mod state_builders;

// AsyncRouter（L5：物理迁入 peri-agent，仅依赖契约层；本处 re-export 桥保兼容）。
pub use peri_agent::session::async_router::AsyncRouter;

pub use retry_events::RetryEventForwarder;

use std::{
    collections::HashMap,
    sync::{atomic::AtomicBool, atomic::AtomicU8, Arc},
};

use chrono::Utc;
use dashmap::DashMap;
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::{
    store::ThreadStore,
    thread::{ThreadId, ThreadMeta},
};
use tokio_util::sync::CancellationToken;

use peri_acp_types::PeriCaps;

use executor::PERMISSION_MODE_NEVER_NOTIFIED;

use crate::{
    provider::{config::PeriConfig, LlmProvider},
    session::cron_bridge::SessionCronBridge,
};
use peri_acp_types::session::AgentRuntime;

/// 后台任务管理器工厂（装配注入面）：session 创建时调用一次，产出 per-session
/// 的 `Arc<dyn TaskManager>`（Agent 层 per-session 聚合：registry + bg shell 执行；
/// 随 session 创建/销毁）。实现类由部署装配点提供（host 装配面），ACP 协议面
/// 只持有契约 `peri_acp_types::tasks::TaskManager`。
pub type TaskManagerFactory =
    Arc<dyn Fn() -> Arc<dyn peri_acp_types::tasks::TaskManager> + Send + Sync>;

pub struct AcpSession {
    pub session_id: String,
    pub thread_id: ThreadId,
    pub cwd: String,
    pub cancel_token: CancellationToken,
    pub state_messages: Vec<BaseMessage>,
    pub created_at: chrono::DateTime<Utc>,
    /// 当前激活的 provider ID（对应 PeriConfig.config.providers 中的 id）
    pub provider_id: String,
    /// 当前激活的模型别名（"opus"/"sonnet"/"haiku"）
    pub model_alias: String,
    /// 每会话独立的权限模式
    pub permission_mode: Arc<SharedPermissionMode>,
    /// 最近一次已通知模型的 PermissionMode（跨 turn 持久）。
    ///
    /// D2：mode 会话内切换后，executor 在下一可消费 turn 以受控 runtime
    /// event 通知模型，不重建 frozen system prompt。此原子值记录"上次已随
    /// 消息入队通知的 mode"；初始化为 [`PERMISSION_MODE_NEVER_NOTIFIED`]
    /// 哨兵，使首个模型可见 turn 向模型公开初始 mode（10_hitl 不含 mode
    /// snapshot、Bypass 时不渲染 10_hitl），随后 mode 切换各通知一次。
    pub last_notified_permission_mode: Arc<AtomicU8>,
    /// 运行时 agent 实例（根 agent + 子 agent）
    pub active_agents: HashMap<ThreadId, AgentRuntime>,
    /// Goal steering 状态（session 级，跨 prompt 共享）
    pub goal_state: crate::session::goal_state::GoalState,
    /// 统一收件箱（session 级共享，所有路径用）
    ///
    /// v2 stages 使用独立类型
    /// `peri_acp_types::session::MessageQueue`（富类型，带 Kind/Source）。
    /// 每轮 v2 路径调用 `build_stage_context` 时传入此实例的 clone，
    /// 让 main agent 与 SubAgent / Hook / GoalSteering 互可见彼此的
    /// deferred / info 消息。
    ///
    /// 内部 `Arc<Mutex<VecDeque>> + Arc<Notify>`，clone 共享底层。
    pub v2_message_queue: peri_acp_types::session::MessageQueue,
    /// Session-level inbox (await-wake wrapper around v2_message_queue).
    ///
    /// Created lazily on first access via `SessionManager::session_inbox_for`.
    /// Used by the executor to block during idle (`await_wake`) and by
    /// `AsyncRouter` to push bg_results/workflow events with wake notification.
    ///
    /// `None` means the session doesn't support async wake (e.g., print mode
    /// without a SessionManager). The executor falls back to direct return.
    pub session_inbox: Option<Arc<peri_acp_types::session::SessionInbox>>,
    /// Session 级 cron bridge（lazy-init，跨 turn 存活；close_session 时随本结构 drop）。
    pub cron_bridge: Option<crate::session::cron_bridge::SessionCronBridge>,
    /// 后台任务管理器（Agent 层 per-session 聚合：registry + bg shell 执行；
    /// 随 session 创建/销毁，close_session 时 cancel_all 取消 owned 任务）
    pub task_manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    /// idle-suspended 标志：executor 在 await_wake 挂起期间置 true（跨 turn
    /// 持久，Arc 共享）。宿主 `dispatch_prompt_turn` 据此把挂起期间到达的
    /// 用户 prompt 注入 inbox 唤醒 loop（而非在 prompt lock 上阻塞）。
    pub idle_suspended: Arc<AtomicBool>,
}

struct SessionManagerInner {
    sessions: DashMap<String, AcpSession>,
    thread_store: Arc<dyn ThreadStore>,
    provider: LlmProvider,
    peri_config: Arc<PeriConfig>,
    permission_mode: Arc<SharedPermissionMode>,
    /// Global agent overrides from CLI --agent flag (applied to all sessions)
    pub agent_overrides: Option<AgentOverrides>,
    /// initialize 阶段暂存的 peri caps（尚未关联到具体 session）。
    /// session/new 时 clone 写入 caps_registry；协商值保留（不再清空），
    /// 供同一 server 进程内第 2+ 个 session 复用（S1.1，防 stdio 多 session 门控错乱）。
    pub pending_caps: parking_lot::Mutex<Option<PeriCaps>>,
    /// Peri 自定义能力注册表（per-session）。
    /// Key: session_id。使用 Arc<DashMap<...>> 以支持 clone 共享。
    pub caps_registry: Arc<DashMap<String, PeriCaps>>,
    /// 全局 CronScheduler（TUI/stdio 进程共享）。None = 不启用 cron 注入。
    pub cron_scheduler: Option<Arc<dyn peri_acp_types::cron::CronSchedulerPort>>,
    /// Skills 扫描端口（装配注入；frozen 数据构建的 agents/skills 扫描经此访问）。
    pub skills: Arc<dyn peri_acp_types::ports::SkillsPort>,
    /// 后台任务管理器工厂（装配注入面）：每次 session 创建时调用一次，产出
    /// per-session 的 `Arc<dyn TaskManager>`（Agent 层 per-session 聚合）。
    /// None = 未注入时 fallback `NoopTaskManager`（print 等无 bg 场景）。
    pub task_manager_factory: Option<TaskManagerFactory>,
}

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<SessionManagerInner>,
}

impl SessionManager {
    #[allow(clippy::too_many_arguments)] // 装配注入面：端口/工厂逐项注入，L5 装配迁出后可分组
    pub fn new(
        thread_store: Arc<dyn ThreadStore>,
        provider: LlmProvider,
        peri_config: Arc<PeriConfig>,
        permission_mode: Arc<SharedPermissionMode>,
        agent_overrides: Option<AgentOverrides>,
        cron_scheduler: Option<Arc<dyn peri_acp_types::cron::CronSchedulerPort>>,
        task_manager_factory: Option<TaskManagerFactory>,
        skills: Arc<dyn peri_acp_types::ports::SkillsPort>,
    ) -> Self {
        Self {
            inner: Arc::new(SessionManagerInner {
                sessions: DashMap::new(),
                thread_store,
                provider,
                peri_config,
                permission_mode,
                agent_overrides,
                pending_caps: parking_lot::Mutex::new(None),
                caps_registry: Arc::new(DashMap::new()),
                cron_scheduler,
                skills,
                task_manager_factory,
            }),
        }
    }

    /// 构造 per-session 后台任务管理器（装配注入的工厂调用一次；未注入时
    /// fallback `NoopTaskManager`——print 等无 bg 场景）。
    fn make_task_manager(&self) -> Arc<dyn peri_acp_types::tasks::TaskManager> {
        self.inner
            .task_manager_factory
            .as_ref()
            .map(|f| f())
            .unwrap_or_else(|| Arc::new(peri_acp_types::tasks::NoopTaskManager))
    }

    /// 使用指定 session_id 创建会话（用于 session/load 和 session/resume）
    pub async fn new_session_with_id(&self, session_id: &str, cwd: &str) -> anyhow::Result<()> {
        if self.inner.sessions.contains_key(session_id) {
            return Ok(());
        }

        let thread_id = ThreadId::from(session_id.to_string());
        let session = self.build_session(session_id, thread_id, cwd);

        self.inner.sessions.insert(session_id.to_string(), session);
        Ok(())
    }

    pub async fn new_session(&self, cwd: &str) -> anyhow::Result<(String, ThreadId)> {
        let meta = ThreadMeta::new(cwd);
        let thread_id = self.inner.thread_store.create_thread(meta).await?;

        let session_id = thread_id.clone();

        let session = self.build_session(&session_id, thread_id.clone(), cwd);

        self.inner.sessions.insert(session_id.clone(), session);
        Ok((session_id, thread_id))
    }

    /// 创建新会话并继承指定的 provider_id、model_alias
    pub async fn new_session_with_settings(
        &self,
        cwd: &str,
        provider_id: String,
        model_alias: String,
    ) -> anyhow::Result<(String, ThreadId)> {
        let meta = ThreadMeta::new(cwd);
        let thread_id = self.inner.thread_store.create_thread(meta).await?;

        let session_id = thread_id.clone();

        let task_manager = self.make_task_manager();

        let session = AcpSession {
            session_id: session_id.clone(),
            thread_id: thread_id.clone(),
            cwd: cwd.to_string(),
            cancel_token: CancellationToken::new(),
            state_messages: Vec::new(),
            created_at: Utc::now(),
            provider_id,
            model_alias,
            permission_mode: SharedPermissionMode::new(PermissionMode::AutoMode),
            // 初始化为"未通知过"哨兵：首个模型可见 turn 公开初始 mode（D2，
            // 见 PERMISSION_MODE_NEVER_NOTIFIED）；入队后由 executor 记账。
            last_notified_permission_mode: Arc::new(AtomicU8::new(PERMISSION_MODE_NEVER_NOTIFIED)),
            active_agents: HashMap::new(),
            goal_state: crate::session::goal_state::GoalState::new(
                Arc::new(peri_acp_types::goal::InMemoryGoalStore::new()),
                session_id.clone(),
            ),
            v2_message_queue: peri_acp_types::session::MessageQueue::new(),
            session_inbox: None,
            cron_bridge: None,
            task_manager,
            idle_suspended: Arc::new(AtomicBool::new(false)),
        };

        self.inner.sessions.insert(session_id.clone(), session);
        Ok((session_id, thread_id))
    }

    fn build_session(&self, session_id: &str, thread_id: ThreadId, cwd: &str) -> AcpSession {
        let task_manager = self.make_task_manager();

        AcpSession {
            session_id: session_id.to_string(),
            thread_id,
            cwd: cwd.to_string(),
            cancel_token: CancellationToken::new(),
            state_messages: Vec::new(),
            created_at: Utc::now(),
            provider_id: self
                .inner
                .peri_config
                .config
                .profiles
                .get(&self.inner.peri_config.config.active_alias)
                .map(|p| p.provider.clone())
                .unwrap_or_default(),
            model_alias: self.inner.peri_config.config.active_alias.clone(),
            permission_mode: SharedPermissionMode::new(PermissionMode::AutoMode),
            // 初始化为"未通知过"哨兵：首个模型可见 turn 公开初始 mode（D2，
            // 见 PERMISSION_MODE_NEVER_NOTIFIED）；入队后由 executor 记账。
            last_notified_permission_mode: Arc::new(AtomicU8::new(PERMISSION_MODE_NEVER_NOTIFIED)),
            active_agents: HashMap::new(),
            goal_state: crate::session::goal_state::GoalState::new(
                Arc::new(peri_acp_types::goal::InMemoryGoalStore::new()),
                session_id.to_string(),
            ),
            v2_message_queue: peri_acp_types::session::MessageQueue::new(),
            session_inbox: None,
            cron_bridge: None,
            task_manager,
            idle_suspended: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn close_session(&self, session_id: &str) -> anyhow::Result<()> {
        if let Some((_, session)) = self.inner.sessions.remove(session_id) {
            // 取消所有运行时 agent 实例（终止执行归 Agent 层，L5）
            peri_acp_types::session::cancel_all_agents(session.active_agents.values());
            session.cancel_token.cancel();
            // L1：取消 owned 后台任务（§9 销毁顺序「取消 owned tasks」：
            // bg shell / bg agent / workflow 随 session 销毁，多会话互不干扰）
            session.task_manager.cancel_all();
        }
        Ok(())
    }

    pub async fn list_sessions(&self) -> anyhow::Result<Vec<ThreadMeta>> {
        self.inner.thread_store.list_threads().await
    }

    pub fn get_session(
        &self,
        session_id: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, AcpSession>> {
        self.inner.sessions.get(session_id)
    }

    pub fn get_session_mut(
        &self,
        session_id: &str,
    ) -> Option<dashmap::mapref::one::RefMut<'_, String, AcpSession>> {
        self.inner.sessions.get_mut(session_id)
    }

    pub fn inner_sessions(&self) -> &DashMap<String, AcpSession> {
        &self.inner.sessions
    }

    pub fn cancel_session(&self, session_id: &str) {
        if let Some(mut session) = self.inner.sessions.get_mut(session_id) {
            // Cascade/Independent 判定与终止执行归 Agent 层（L5：cancel 最终
            // 执行权在 Agent，top-level.md §2/§9）；此处仅定位并传递注册表。
            peri_acp_types::session::cancel_cascade_agents(session.active_agents.values());

            // Cancel the current token so all clones (held by link tasks,
            // permission loops) detect cancellation. Then replace with a fresh
            // token so subsequent prompts on the same session are not affected.
            // CancellationToken has no reset() — once cancelled it stays cancelled.
            session.cancel_token.cancel();
            session.cancel_token = CancellationToken::new();
        }
    }

    pub fn provider(&self) -> &LlmProvider {
        &self.inner.provider
    }

    pub fn peri_config(&self) -> &Arc<PeriConfig> {
        &self.inner.peri_config
    }

    pub fn permission_mode(&self) -> &Arc<SharedPermissionMode> {
        &self.inner.permission_mode
    }

    pub fn thread_store(&self) -> &Arc<dyn ThreadStore> {
        &self.inner.thread_store
    }

    pub fn agent_overrides(&self) -> Option<&AgentOverrides> {
        self.inner.agent_overrides.as_ref()
    }

    /// initialize handler 调用：暂存 clientCapabilities 中的 peri caps。
    pub fn set_pending_caps(&self, caps: PeriCaps) {
        *self.inner.pending_caps.lock() = Some(caps);
    }

    /// 查询 initialize 是否已被调用（pending_caps 是否被设置过）。
    /// 用于 MpscTransport 路径判断：若未调用 initialize，默认全部 cap=true。
    pub fn pending_caps_was_set(&self) -> bool {
        self.inner.pending_caps.lock().is_some()
    }

    /// session/new 时调用：将暂存的 caps 关联到 session_id，返回 caps 副本。
    /// 如果 initialize 时未声明任何 caps，返回默认值（全 false）。
    ///
    /// S1.1：改为 clone 而非 take —— 协商值是 server 进程级配置（initialize 只
    /// 调用一次），必须保留供第 2+ 个 session 复用；否则 stdio 第 2 个
    /// session/new 会取到 None 注册全 false caps。
    pub fn consume_pending_caps(&self, session_id: &str) -> PeriCaps {
        let caps = self.inner.pending_caps.lock().clone().unwrap_or_default();
        self.inner
            .caps_registry
            .insert(session_id.to_string(), caps.clone());
        caps
    }

    /// Sending point 调用：读取 session 的 peri caps。
    /// 未设置时返回默认值（全 false）。
    pub fn get_caps(&self, session_id: &str) -> PeriCaps {
        self.inner
            .caps_registry
            .get(session_id)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// 获取 caps_registry 的 Arc clone，用于传递给 TransportEventSink
    /// 等需独立访问 registry 的组件。
    pub fn caps_registry(&self) -> Arc<DashMap<String, PeriCaps>> {
        self.inner.caps_registry.clone()
    }

    /// 确保指定 session 的 caps 已在 registry 中注册。
    ///
    /// - registry 已有条目 → 直接返回（幂等）。
    /// - 已协商过（`pending_caps_was_set()`，stdio 路径经过 initialize）→
    ///   用协商值 clone 并写入。
    /// - 未协商（MpscTransport / TUI 内部路径，无 initialize）→ 写入 `all_enabled()`。
    ///
    /// S1.1：协商值不再被 consume 清空，因此 load/resume/fork 新 session id
    /// 也能拿到协商值；未协商才回退 all_enabled（与 `consume_pending_caps`
    /// 未协商回退全 false 的语义刻意不同，两侧不可互换）。
    ///
    /// 幂等：重复调用不会覆盖已有值。
    /// 与 `consume_pending_caps` 的 lock 独立操作，避免 TOCTOU 竞态。
    pub fn ensure_session_caps(&self, session_id: &str) -> PeriCaps {
        // 已有注册 → 直接返回（幂等）
        if let Some(caps) = self.inner.caps_registry.get(session_id) {
            return caps.clone();
        }
        // 协商过 → 用协商值 clone；未协商 → 默认全启用。
        // pending_caps 只被 set 从不被 take/clear，was_set 检查后 clone 无竞态。
        let caps = if self.pending_caps_was_set() {
            self.inner.pending_caps.lock().clone().unwrap_or_default()
        } else {
            PeriCaps::all_enabled()
        };
        self.inner
            .caps_registry
            .insert(session_id.to_string(), caps.clone());
        caps
    }

    /// 构建会话级 frozen 数据（统一构造入口，消除 TUI/stdio 重复 5 处）。
    ///
    /// `workflow_enabled`：会话创建时 Workflow executor 是否可用。
    /// TUI/stdio 正常路径恒为 true（prompt 执行时无条件创建 executor）；
    /// print mode 不经过此入口（直接调 `FrozenSessionData::build` 传 false）。
    ///
    /// L5：渲染面（CLAUDE.md 解析 / skills 摘要 / prompt 模板）随
    /// `FrozenSessionData::build` 留在 ACP（§0 渲染是 ACP 协议面职责），
    /// 类型经 `from_frozen_parts` 装配（peri-agent 侧不可变数据存储）。
    pub fn build_frozen_data(
        &self,
        cwd: &str,
        plugin_skill_roots: &[peri_acp_types::skills::SkillRoot],
        plugin_agent_dirs: &[std::path::PathBuf],
        workflow_enabled: bool,
    ) -> crate::session::executor::FrozenSessionData {
        let frozen_date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let frozen_language = self.inner.peri_config.config.language.clone();
        let pm = self.inner.permission_mode.load();
        let (claude_md, claude_local_md) =
            peri_middlewares::AgentsMdMiddleware::read_frozen_content(cwd);
        // 一次性读取 disableBundledSkills 并冻结到 frozen_skill_summary
        // （保持系统提示词稳定性：会话内不重读）
        let disable_bundled = peri_middlewares::skills::load_disable_bundled_skills();
        let skill_summary = peri_middlewares::SkillsMiddleware::build_frozen_summary(
            cwd,
            plugin_skill_roots.to_vec(),
            disable_bundled,
        );

        let features = crate::prompt::PromptFeatures::detect(pm, workflow_enabled);
        let template = crate::prompt::PromptTemplate::new();
        let env = crate::prompt::PromptEnv::with_frozen_date(cwd, &frozen_date);
        let system_prompt = template.render(
            &env,
            &features,
            self.inner.skills.as_ref(),
            plugin_agent_dirs,
            frozen_language.as_deref(),
        );

        // 子 agent / fork / workflow agent 复用的冻结 prompt（P2-2026-08-02）：
        // 这些链不注册 WorkflowTool（shared_tools: None），主链冻结 prompt 中
        // 的 16_workflow section 不得被 fork 继承或 workflow agent 复用。
        // 仅在 workflow_enabled 时多渲染一次（session 创建时一次性，不违反
        // ARC-FROZEN-001 的每 turn 重建禁令）；workflow 关闭时两版相同。
        let subagent_system_prompt = if workflow_enabled {
            let sub_features = crate::prompt::PromptFeatures::detect_without_workflow(pm);
            Some(Arc::from(template.render(
                &env,
                &sub_features,
                self.inner.skills.as_ref(),
                plugin_agent_dirs,
                frozen_language.as_deref(),
            )))
        } else {
            None
        };

        // 构建 v2 FrozenContext
        let v2_frozen = peri_agent::session::FrozenContext {
            system_prompt: Arc::from(system_prompt),
            claude_md: claude_md.map(Arc::from).unwrap_or_default(),
            skill_summary: skill_summary.map(Arc::from).unwrap_or_default(),
            date: Arc::from(frozen_date),
            language: frozen_language.map(|l| Arc::from(l.to_string())),
        };

        crate::session::executor::FrozenSessionData::from_frozen_parts(
            v2_frozen,
            claude_local_md.map(Arc::from),
            subagent_system_prompt,
        )
    }

    /// 确保指定 session 在 SessionManager 中存在 AcpSession 记录，
    /// 用于支撑 cascade cancel 子 agent 与 goal_state 跨 prompt 共享。
    ///
    /// 如果 session 已存在则 no-op；否则插入一个空 history 的 AcpSession。
    /// TUI/stdio 调用方仍自行维护 history/frozen/agent_pool 等字段，
    /// SessionManager 只负责 active_agents / goal_state 维度。
    pub fn ensure_session(&self, session_id: &str, cwd: &str) {
        if self.inner.sessions.contains_key(session_id) {
            return;
        }
        let thread_id = ThreadId::from(session_id.to_string());
        let session = self.build_session(session_id, thread_id, cwd);
        self.inner.sessions.insert(session_id.to_string(), session);
    }

    /// 取指定 session 的 goal_state 句柄（用于 TUI/stdio 注入到 middleware 链）。
    ///
    /// 调用方应先调用 [`ensure_session`] 保证记录存在。
    /// 不存在时返回 None。
    pub fn goal_state_for(
        &self,
        session_id: &str,
    ) -> Option<crate::session::goal_state::GoalState> {
        self.inner
            .sessions
            .get(session_id)
            .map(|s| s.goal_state.clone())
    }

    /// 获取指定 session 的共享 v2 MessageQueue（用于 TUI 侧 cron/channel 异步触发注入）。
    /// 内部 Arc 共享，clone 廉价。session 不存在时返回 None。
    pub fn v2_queue_for(&self, session_id: &str) -> Option<peri_acp_types::session::MessageQueue> {
        self.inner
            .sessions
            .get(session_id)
            .map(|s| s.v2_message_queue.clone())
    }

    /// 获取指定 session 的 SessionInbox（await-wake wrapper）。
    ///
    /// Lazy-init：首次调用时创建 `SessionInbox` 包装该 session 的
    /// `v2_message_queue`，存入 `AcpSession.session_inbox` 后续调用直接返回。
    /// session 不存在时返回 None。
    pub fn session_inbox_for(
        &self,
        session_id: &str,
    ) -> Option<Arc<peri_acp_types::session::SessionInbox>> {
        // Fast path: already initialized
        if let Some(session) = self.inner.sessions.get(session_id) {
            if let Some(ref inbox) = session.session_inbox {
                return Some(Arc::clone(inbox));
            }
        }
        // Slow path: lazy init
        if let Some(mut session) = self.inner.sessions.get_mut(session_id) {
            let queue_arc = Arc::new(session.v2_message_queue.clone());
            let inbox = Arc::new(peri_acp_types::session::SessionInbox::new(queue_arc));
            session.session_inbox = Some(Arc::clone(&inbox));
            Some(inbox)
        } else {
            None
        }
    }

    /// 读取 session 的 idle-suspended 标志（await_wake 挂起期间为 true）。
    ///
    /// 宿主 `dispatch_prompt_turn` 在等待 per-session prompt lock 之前检查：
    /// 挂起中到达的用户 prompt 应注入 inbox 唤醒 loop，而不是在锁上阻塞
    /// 至当前 turn 完成。session 不存在时返回 false（走正常排队路径）。
    pub fn is_idle_suspended(&self, session_id: &str) -> bool {
        self.inner
            .sessions
            .get(session_id)
            .map(|s| s.idle_suspended.load(std::sync::atomic::Ordering::Acquire))
            .unwrap_or(false)
    }

    /// 确保指定 session 的 session 级 cron bridge 已启动（lazy-init，幂等）。
    ///
    /// 首次调用：`scheduler.subscribe()` 一次 + 用 session 级 inbox handle 启动
    /// `SessionCronBridge`。此后每 turn 重复调用均为 no-op（"already set" 检查在
    /// `get_mut` 写锁内，杜绝并发双订阅）。bridge 跨 turn 存活，close_session
    /// 时随 AcpSession drop 而中止。
    ///
    /// session 不存在或 scheduler 未配置时返回 false。
    pub fn cron_bridge_for(&self, session_id: &str) -> bool {
        let scheduler = match &self.inner.cron_scheduler {
            Some(s) => s.clone(),
            None => return false,
        };
        // Fast path
        if let Some(session) = self.inner.sessions.get(session_id) {
            if session.cron_bridge.is_some() {
                return true;
            }
        }
        // Slow path: session-level inbox first (shared wake Notify), then bridge
        let Some(inbox) = self.session_inbox_for(session_id) else {
            return false;
        };
        if let Some(mut session) = self.inner.sessions.get_mut(session_id) {
            if session.cron_bridge.is_none() {
                session.cron_bridge = Some(SessionCronBridge::start(&scheduler, inbox.handle()));
                tracing::info!(session_id = %session_id, "session cron bridge started");
            }
        }
        // 理论性竞态：session_inbox_for 成功后 get_mut 返回 None（并发
        // close_session 恰好移除）时返回 true 但未实际创建——生产调用方忽略返回值。
        true
    }

    /// 取消指定 session 的所有 cascade 子 agent（暴露给 TUI/stdio 用于 session/cancel）。
    pub fn cancel_cascade_children_for(&self, session_id: &str) {
        if let Some(session) = self.inner.sessions.get(session_id) {
            session.cancel_cascade_children();
        }
    }
}

impl AcpSession {
    /// 取消指定 agent 的所有 cascade 子 agent。
    ///
    /// 薄委托：Cascade/Independent 判定与终止执行归 Agent 层
    /// （`peri_acp_types::session::cancel_cascade_agents`，L5 cancel
    /// 最终执行权归位）；本方法仅定位（持有 active_agents 注册表）。
    pub fn cancel_cascade_children(&self) {
        peri_acp_types::session::cancel_cascade_agents(self.active_agents.values());
    }

    /// 取消所有 agent（session 结束时）
    pub fn cancel_all_agents(&self) {
        peri_acp_types::session::cancel_all_agents(self.active_agents.values());
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

// ── L5：executor 对 SessionManager 的访问端口实现 ──────────────────────────

use std::str::FromStr;

use peri_acp_types::session::SessionAccessPort;
use peri_acp_types::thread::CancelPolicy;

impl SessionAccessPort for SessionManager {
    fn v2_message_queue(&self, session_id: &str) -> Option<peri_acp_types::session::MessageQueue> {
        self.v2_queue_for(session_id)
    }

    fn session_inbox(
        &self,
        session_id: &str,
    ) -> Option<Arc<peri_acp_types::session::SessionInbox>> {
        self.session_inbox_for(session_id)
    }

    fn idle_suspended_flag(&self, session_id: &str) -> Option<Arc<AtomicBool>> {
        self.inner
            .sessions
            .get(session_id)
            .map(|s| s.idle_suspended.clone())
    }

    fn task_manager(
        &self,
        session_id: &str,
    ) -> Option<Arc<dyn peri_acp_types::tasks::TaskManager>> {
        self.inner
            .sessions
            .get(session_id)
            .map(|s| s.task_manager.clone())
    }

    fn last_notified_permission_mode(&self, session_id: &str) -> Option<Arc<AtomicU8>> {
        self.inner
            .sessions
            .get(session_id)
            .map(|s| s.last_notified_permission_mode.clone())
    }

    fn goal_controller(
        &self,
        session_id: &str,
    ) -> Option<Arc<dyn peri_acp_types::goal::GoalController>> {
        self.goal_state_for(session_id)
            .map(|gs| Arc::new(gs) as Arc<dyn peri_acp_types::goal::GoalController>)
    }

    fn register_runtime(
        &self,
        session_id: &str,
    ) -> Option<peri_acp_types::frozen::RegisterRuntimeFn> {
        // 原 executor 语义：SessionManager 存在即返回闭包（session 不存在时
        // 闭包内静默跳过，不注册）。
        let sm = self.clone();
        let sid = session_id.to_string();
        Some(Arc::new(
            move |thread_id: String, cancel_token: CancellationToken, policy: String| {
                if let Some(mut session) = sm.get_session_mut(&sid) {
                    // policy 字符串（"cascade"/"independent"）来自 SubAgentMiddleware；
                    // 契约类型 FromStr 对非法值报错，此处保留迁移前 `_ => Cascade`
                    // 的容错语义（Default = Cascade）。
                    let cancel_policy = CancelPolicy::from_str(&policy).unwrap_or_default();
                    let runtime = AgentRuntime::new(thread_id.clone(), cancel_policy);
                    // Store the provided cancel_token so external cancellation works
                    let rt = AgentRuntime {
                        thread_id,
                        cancel_token,
                        cancel_policy: runtime.cancel_policy,
                        status: runtime.status,
                    };
                    session.active_agents.insert(rt.thread_id.clone(), rt);
                }
            },
        ))
    }

    fn deregister_runtime(
        &self,
        session_id: &str,
    ) -> Option<peri_acp_types::frozen::DeregisterRuntimeFn> {
        let sm = self.clone();
        let sid = session_id.to_string();
        Some(Arc::new(move |thread_id: &str| {
            if let Some(mut session) = sm.get_session_mut(&sid) {
                session.active_agents.remove(thread_id);
            }
        }))
    }

    fn cancel_cascade_children(&self, session_id: &str) {
        self.cancel_cascade_children_for(session_id);
    }

    fn cron_bridge_for(&self, session_id: &str) -> bool {
        SessionManager::cron_bridge_for(self, session_id)
    }
}
