//! 会话初始状态与命令注册装配；不发布或拥有 session 注册表。

use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc};

use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::thread::ThreadId;
use tokio_util::sync::CancellationToken;

use super::{AcpSession, SessionManager};

impl SessionManager {
    /// 构造 per-session 后台任务管理器（装配注入的工厂调用一次；未注入时
    /// fallback `NoopTaskManager`——print 等无 bg 场景）。
    ///
    /// 这是**唯一**的工厂调用点：会话创建路径（[`Self::build_session`]）与会话环境
    /// 装配（`peri-acp/src/host/workspace.rs` 的 `SessionEnvironment::assemble`，
    /// AW3-11：产生点必须在池的 `run_initialize` 之前）都经它取 manager，避免
    /// 「工厂语义」出现两份。
    pub(crate) fn new_session_task_manager(&self) -> Arc<dyn peri_acp_types::tasks::TaskManager> {
        self.inner
            .task_manager_factory
            .as_ref()
            .map(|f| f())
            .unwrap_or_else(|| Arc::new(peri_acp_types::tasks::NoopTaskManager))
    }

    /// 构建 per-session 命令注册表（Phase 6 B2/C1 注册顺序契约）：
    ///
    /// 1. 内置命令（[`register_builtins`]，先注册者占键——内置永远优先，
    ///    设计 §64 冲突纯拒绝 + 装配顺序裁决）；
    /// 2. 插件静态命令（B2：`plugin:{plugin}:{cmd}` 三层形态，kind =
    ///    Command，provenance = Plugin{name} + Connected）。
    ///
    /// W4b（F6/J5）：原「本地 skills 同步扫盘注册 `core:{name}`」已删除——技能
    /// 目录的唯一来源是会话级 MCP skill registry。`core:{skill}` 裸名命令由 MCP
    /// 发现管线**异步投影**（peri-middlewares 的 `mcp::skill_discovery` 中的
    /// `project_core_skill_commands`，随 `/server:skill` 同批写入；实例关闭或断连
    /// 时同批撤下），因此这里不再需要技能扫描，也不再需要在构造期读取
    /// `SkillsMiddleware` 关闭位（关闭位由发现面按 A24 关闭集 +
    /// `SkillsMiddleware` 关闭位（`BuiltinInstanceContext.skills_face_closed`，
    /// 与关闭集同一份 `disabled_middlewares` 派生）消费）。
    ///
    /// 动态注入（MCP / 插件运行时注册注销）由发现管线异步驱动（A3 已接，
    /// 不在此处）。skill 注入语义（`AgentPassthrough`）与插件命令执行语义
    /// 均为占位（Phase 5+ 补齐执行体）。
    fn build_command_registry(&self) -> Arc<CommandRegistry> {
        let reg = Arc::new(CommandRegistry::new());
        // 1) 内置（先注册者占键，后续同键一律 Conflict 拒绝）。
        crate::session::command::register_builtins(&reg);
        // 2) 插件静态命令（B2；bare 时为空 Vec，注册零条目无副作用）。
        reg.register_all(self.inner.plugin_command_entries.clone());
        reg
    }

    pub(super) fn build_session(
        &self,
        session_id: &str,
        thread_id: ThreadId,
        cwd: &str,
    ) -> AcpSession {
        self.build_session_with_task_manager(session_id, thread_id, cwd, None)
    }

    /// 同 [`Self::build_session`]，但允许调用方**携带外部** `TaskManager`（AW3-11：
    /// 会话环境装配先产出 manager 并送进 builtin 上下文，登记会话时用同一份
    /// `Arc`；`None` 走工厂 / `NoopTaskManager` fallback）。
    pub(super) fn build_session_with_task_manager(
        &self,
        session_id: &str,
        thread_id: ThreadId,
        cwd: &str,
        task_manager: Option<Arc<dyn peri_acp_types::tasks::TaskManager>>,
    ) -> AcpSession {
        let task_manager = task_manager.unwrap_or_else(|| self.new_session_task_manager());

        AcpSession {
            session_id: session_id.to_string(),
            thread_id,
            cwd: cwd.to_string(),
            cancel_token: CancellationToken::new(),
            state_messages: Vec::new(),
            created_at: peri_time::now_wall().into(),
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
            active_agents: HashMap::new(),
            goal_state: crate::session::goal_state::GoalState::new(
                Arc::new(peri_acp_types::goal::InMemoryGoalStore::new()),
                session_id.to_string(),
            ),
            v2_message_queue: peri_acp_types::session::MessageQueue::new(),
            activation: Arc::new(super::SessionActivation::default()),
            session_inbox: None,
            user_input_mailbox: None,
            user_input_events_cancel: CancellationToken::new(),
            cron_bridge: None,
            task_manager,
            task_events_started: std::sync::atomic::AtomicBool::new(false),
            task_events_cancel: CancellationToken::new(),
            idle_suspended: Arc::new(AtomicBool::new(false)),
            mcp_skill_registry: Arc::new(McpSkillRegistry::new()),
            command_registry: self.build_command_registry(),
            mcp_subscription: self.inner.mcp_subscription.clone(),
            dynamic_mcp_deployment: self.inner.dynamic_mcp.clone(),
            dynamic_mcp_close: self
                .inner
                .dynamic_mcp
                .as_ref()
                .map(|deployment| deployment.close_registration(session_id)),
            dynamic_mcp_projection: Arc::new(parking_lot::Mutex::new(None)),
            dynamic_mcp_notifications: None,
        }
    }
}
