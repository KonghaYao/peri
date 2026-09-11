//! ACP session 注册表、外部句柄与生命周期边界。
//!
//! `SessionManager` 的 clone 共享唯一 `SessionManagerInner`；本模块保留
//! session 发布、关闭、取消与注册表访问，私有子模块分别实现初始装配、
//! capability 协商、frozen 渲染、executor 访问及 Cron/MCP 接线。
//!
//! `AcpSession` 持有会话的 active agent 句柄、收件箱、后台任务管理器与
//! bridge/projection lease；取消策略和终止执行仍委托给契约层
//! `cancel_cascade_agents` / `cancel_all_agents`。Cron bridge 跨 turn 存活，
//! session 关闭时随 owner 释放。

mod access;
pub mod agent_pool;
mod bridges;
mod caps;
pub mod command;
mod construction;
pub mod cron_bridge;
mod dynamic_mcp;
pub mod event_sink;
pub mod executor;
mod frozen;
pub(crate) mod frozen_snapshot;
pub mod goal_state;
pub mod retry_events;
pub mod state_builders;

// AsyncRouter（L5：物理迁入 peri-agent，仅依赖契约层；本处 re-export 桥保兼容）。
pub use peri_agent::session::async_router::AsyncRouter;

pub use dynamic_mcp::SessionDynamicMcpNotificationSink;
pub(crate) use frozen::build_collected_sections;
pub use retry_events::RetryEventForwarder;

use std::{
    collections::HashMap,
    sync::{atomic::AtomicBool, Arc},
};

use chrono::Utc;
use dashmap::DashMap;
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::command::command_route::RouteEntry;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::skills::SkillRoot;
use peri_acp_types::{
    store::ThreadStore,
    thread::{ThreadId, ThreadMeta},
};
use tokio_util::sync::CancellationToken;

use peri_acp_types::PeriCaps;

use crate::provider::{config::PeriConfig, LlmProvider};
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
    /// Agent-owned user input lifecycle, shared across prompt attempts.
    pub(crate) user_input_mailbox:
        Option<Arc<peri_agent::session::user_input_mailbox::UserInputMailbox>>,
    pub(crate) user_input_events_cancel: CancellationToken,
    /// Session 级 cron bridge（lazy-init，跨 turn 存活；close_session 时随本结构 drop）。
    pub cron_bridge: Option<crate::session::cron_bridge::SessionCronBridge>,
    /// 后台任务管理器（Agent 层 per-session 聚合：registry + bg shell 执行；
    /// 随 session 创建/销毁，close_session 时 cancel_all 取消 owned 任务）
    pub task_manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    /// idle-suspended 标志：executor 在 await_wake 挂起期间置 true（跨 turn
    /// 持久，Arc 共享）。宿主 `dispatch_prompt_turn` 据此把挂起期间到达的
    /// 用户 prompt 注入 inbox 唤醒 loop（而非在 prompt lock 上阻塞）。
    pub idle_suspended: Arc<AtomicBool>,
    /// Session 级 MCP skill 远端注册表（发现任务写入，Skills 侧读取合并；
    /// 随本结构 drop 释放，杜绝全局挂点——验收 14）。
    pub mcp_skill_registry: Arc<McpSkillRegistry>,
    /// Session 级命令注册表（随 session 创建初始化并注册内置命令；跨轮常驻，
    /// 动态注入条目不因轮次丢失；随本结构 drop 释放，杜绝全局挂点）。
    pub command_registry: Arc<CommandRegistry>,
    /// Idempotent Dynamic MCP cleanup lease.
    pub dynamic_mcp_close: Option<Arc<dyn peri_acp_types::ports::SessionCloseRegistration>>,
    /// Strong owner for the checked session-local MCP projection across stage builds.
    pub dynamic_mcp_projection:
        Arc<parking_lot::Mutex<Option<Arc<dyn peri_acp_types::ports::SessionMcpProjectionLease>>>>,
    /// Strong owner for the checked weak Dynamic MCP notification sink.
    pub dynamic_mcp_notifications: Option<Arc<SessionDynamicMcpNotificationSink>>,
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
    /// Host 统一 continuation scheduler 的 cron 入口；server 启动后绑定。
    pub cron_continuation_tx: parking_lot::Mutex<
        Option<tokio::sync::mpsc::UnboundedSender<peri_acp_types::cron::CronContinuationRequest>>,
    >,
    /// MCP subscriptions 桥接端口（装配注入；session 创建时注册 inbox，
    /// close_session 时注销——订阅通知唤醒 agent 的通道，同 cron 模式）。
    pub mcp_subscription: Option<Arc<dyn peri_acp_types::mcp::McpSubscriptionPort>>,
    /// Deployment-level Dynamic MCP state machine port.
    pub dynamic_mcp: Option<Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort>>,
    /// Skills 扫描端口（装配注入；frozen 数据构建的 agents/skills 扫描经此访问）。
    pub skills: Arc<dyn peri_acp_types::ports::SkillsPort>,
    /// 插件命令静态条目（Phase 6 B2 预转；会话创建时按
    /// 内置 → 本地 skills（C1）→ 插件 顺序 register_all）。
    pub plugin_command_entries: Vec<RouteEntry>,
    /// 插件 skill roots（C1 本地 skills 扫描参数；与 host cfg 同源）。
    pub plugin_skill_roots: Vec<SkillRoot>,
    /// 后台任务管理器工厂（装配注入面）：每次 session 创建时调用一次，产出
    /// per-session 的 `Arc<dyn TaskManager>`（Agent 层 per-session 聚合）。
    /// None = 未注入时 fallback `NoopTaskManager`（print 等无 bg 场景）。
    pub task_manager_factory: Option<TaskManagerFactory>,
}

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<SessionManagerInner>,
}

impl AcpSession {
    /// Close the same resources again after Incomplete; no registry guard crosses await.
    pub(crate) async fn close_resources(
        &self,
    ) -> peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport {
        if let Some(mailbox) = &self.user_input_mailbox {
            mailbox.invalidate();
        }
        self.user_input_events_cancel.cancel();
        if let Some(projection) = self.dynamic_mcp_projection.lock().take() {
            projection.close();
        }
        let report = match &self.dynamic_mcp_close {
            Some(close) => close.revoke_and_cleanup().await,
            None => peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete,
        };
        peri_acp_types::session::cancel_all_agents(self.active_agents.values());
        self.cancel_token.cancel();
        self.task_manager.cancel_all();
        report
    }
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
        mcp_subscription: Option<Arc<dyn peri_acp_types::mcp::McpSubscriptionPort>>,
        dynamic_mcp: Option<Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort>>,
        task_manager_factory: Option<TaskManagerFactory>,
        skills: Arc<dyn peri_acp_types::ports::SkillsPort>,
        plugin_command_entries: Vec<RouteEntry>,
        plugin_skill_roots: Vec<SkillRoot>,
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
                cron_continuation_tx: parking_lot::Mutex::new(None),
                mcp_subscription,
                dynamic_mcp,
                skills,
                plugin_command_entries,
                plugin_skill_roots,
                task_manager_factory,
            }),
        }
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

    pub async fn close_session(&self, session_id: &str) -> anyhow::Result<()> {
        if let Some(session) = self.take_for_close(session_id) {
            let _ = session.close_resources().await;
        }
        Ok(())
    }

    /// Transfer the removed record to the host's retryable exit context.
    pub(crate) fn take_for_close(&self, session_id: &str) -> Option<AcpSession> {
        if let Some(port) = &self.inner.mcp_subscription {
            port.unregister_inbox(session_id);
        }
        self.inner
            .sessions
            .remove(session_id)
            .map(|(_, session)| session)
    }

    /// Begin terminal shutdown without removing the record needed by a
    /// cooperatively unwinding prompt.
    pub(crate) fn pre_close_session(&self, session_id: &str) {
        if let Some(session) = self.inner.sessions.get(session_id) {
            if let Some(mailbox) = &session.user_input_mailbox {
                mailbox.invalidate();
            }
            session.user_input_events_cancel.cancel();
            peri_acp_types::session::cancel_all_agents(session.active_agents.values());
            session.cancel_token.cancel();
            session.task_manager.cancel_all();
        }
    }

    /// Stable session identity snapshot for the host EOF transaction.
    pub(crate) fn session_ids(&self) -> Vec<String> {
        self.inner
            .sessions
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
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
            if let Some(mailbox) = &session.user_input_mailbox {
                mailbox.stop();
            }
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

    /// 确保指定 session 在 SessionManager 中存在 AcpSession 记录，
    /// 用于支撑 cascade cancel 子 agent 与 goal_state 跨 prompt 共享。
    ///
    /// 如果 session 已存在则 no-op；否则插入一个空 history 的 AcpSession。
    /// TUI/stdio 调用方仍自行维护 history/frozen/agent_pool 等字段，
    /// SessionManager 只负责 active_agents / goal_state 维度。
    pub fn ensure_session(&self, session_id: &str, cwd: &str) {
        if !self.inner.sessions.contains_key(session_id) {
            let thread_id = ThreadId::from(session_id.to_string());
            let session = self.build_session(session_id, thread_id, cwd);
            self.inner.sessions.insert(session_id.to_string(), session);
        }
        // 在 session 发布边界立即订阅，避免首个 turn 前到点的 trigger 丢失。
        // cron_bridge_for 本身幂等，既有 session 与 first-turn 调用均不会重复订阅。
        self.cron_bridge_for(session_id);
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
