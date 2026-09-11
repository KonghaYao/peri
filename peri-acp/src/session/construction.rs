//! 会话初始状态与命令注册装配；不发布或拥有 session 注册表。

use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc};

use chrono::Utc;
use peri_acp_types::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
};
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use peri_acp_types::thread::ThreadId;
use tokio_util::sync::CancellationToken;

use super::{AcpSession, SessionManager};

impl SessionManager {
    /// 构造 per-session 后台任务管理器（装配注入的工厂调用一次；未注入时
    /// fallback `NoopTaskManager`——print 等无 bg 场景）。
    fn make_task_manager(&self) -> Arc<dyn peri_acp_types::tasks::TaskManager> {
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
    /// 2. 本地 skills（C1：`core:{name}` 第一等级显式形态，kind = Skill，
    ///    provenance = Core + Connected；同名冲突 / 名含冒号 → 注册表
    ///    `register_all` 内部逐条 warn + 跳过，注册表保持既有条目）；
    /// 3. 插件静态命令（B2：`plugin:{plugin}:{cmd}` 三层形态，kind =
    ///    Command，provenance = Plugin{name} + Connected）。
    ///
    /// 动态注入（MCP / 插件运行时注册注销）由发现管线异步驱动（A3 已接，
    /// 不在此处）。skill 注入语义（`AgentPassthrough`）与插件命令执行语义
    /// 均为占位（Phase 5+ 补齐执行体）。
    fn build_command_registry(&self, cwd: &str) -> Arc<CommandRegistry> {
        let reg = Arc::new(CommandRegistry::new());
        // 1) 内置（先注册者占键，后续同键一律 Conflict 拒绝）。
        crate::session::command::register_builtins(&reg);
        // 2) 本地 skills 归 core 域（C1；扫描调用点收敛为本处——发送侧
        // 旧扫描路径由 Phase 6 C2 收尾清理）。MetaHarness 关闭
        // SkillsMiddleware 时，不得注册 slash 路由，否则 `/skill` 会绕过
        // middleware 装配开关，经 AgentPassthrough 进入 agent 管线。
        let skills_enabled = self
            .inner
            .peri_config
            .config
            .meta_harness
            .as_ref()
            .and_then(|config| config.get("SkillsMiddleware"))
            .copied()
            != Some(false);
        let skills = if skills_enabled {
            self.inner
                .skills
                .available_skills(cwd, &self.inner.plugin_skill_roots)
        } else {
            Vec::new()
        };
        let skill_entries: Vec<RouteEntry> = skills
            .iter()
            .map(|s| RouteEntry {
                // 第一等级显式形态；裸名 = 解析层快捷匹配（alias_index 登记）。
                fullname: format!("core:{}", s.name.to_lowercase()),
                aliases: s.aliases.clone(),
                description: s.description.clone(),
                kind: CommandEntryKind::Skill, // core 域本地 skill（设计 §85）
                category: None,
                args_schema: None,
                handler: Arc::new(crate::session::command::AgentPassthrough), // Phase 1 handler
                provenance: CommandProvenance {
                    source: CommandSource::Core,
                    lifecycle: CommandLifecycle::Connected,
                },
            })
            .collect();
        let (added, errors) = reg.register_all(skill_entries);
        if added < skills.len() {
            tracing::warn!(
                total = skills.len(),
                added,
                errors = ?errors,
                "本地 skills 注册部分失败（同名冲突 / 词法非法，已告警跳过）"
            );
        }
        // 3) 插件静态命令（B2；bare 时为空 Vec，注册零条目无副作用）。
        reg.register_all(self.inner.plugin_command_entries.clone());
        reg
    }

    pub(super) fn build_session(
        &self,
        session_id: &str,
        thread_id: ThreadId,
        cwd: &str,
    ) -> AcpSession {
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
            active_agents: HashMap::new(),
            goal_state: crate::session::goal_state::GoalState::new(
                Arc::new(peri_acp_types::goal::InMemoryGoalStore::new()),
                session_id.to_string(),
            ),
            v2_message_queue: peri_acp_types::session::MessageQueue::new(),
            session_inbox: None,
            user_input_mailbox: None,
            user_input_events_cancel: CancellationToken::new(),
            cron_bridge: None,
            task_manager,
            idle_suspended: Arc::new(AtomicBool::new(false)),
            mcp_skill_registry: Arc::new(McpSkillRegistry::new()),
            command_registry: self.build_command_registry(cwd),
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
