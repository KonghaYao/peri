//! executor 所需的会话访问端口与共享句柄投影。

use std::str::FromStr;
use std::sync::{atomic::AtomicBool, Arc};

use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::session::{AgentRuntime, SessionAccessPort};
use peri_acp_types::thread::CancelPolicy;
use tokio_util::sync::CancellationToken;

use super::SessionManager;

impl SessionManager {
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

    /// 取指定 session 的 MCP skill 远端注册表句柄（SessionAccessPort 投影用）。
    ///
    /// 内部 Arc 共享，clone 廉价；session 不存在时返回 None。
    /// 调用方应先调用 [`ensure_session`] 保证记录存在。
    pub fn mcp_skill_registry_for(&self, session_id: &str) -> Option<Arc<McpSkillRegistry>> {
        self.inner
            .sessions
            .get(session_id)
            .map(|s| s.mcp_skill_registry.clone())
    }

    /// 取指定 session 的命令注册表句柄（命令拦截注入面 / 投影数据源用）。
    ///
    /// 内部 Arc 共享，clone 廉价；session 不存在时返回 None。
    /// 调用方应先调用 [`ensure_session`] 保证记录存在。
    pub fn command_registry_for(&self, session_id: &str) -> Option<Arc<CommandRegistry>> {
        self.inner
            .sessions
            .get(session_id)
            .map(|s| s.command_registry.clone())
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
}
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

    fn mcp_subscription_for(&self, session_id: &str) -> bool {
        SessionManager::mcp_subscription_for(self, session_id)
    }

    fn dynamic_mcp_notifications_for(&self, session_id: &str) -> bool {
        SessionManager::dynamic_mcp_notifications_for(self, session_id)
    }

    fn mcp_skill_registry(&self, session_id: &str) -> Option<Arc<McpSkillRegistry>> {
        self.mcp_skill_registry_for(session_id)
    }

    fn command_registry(&self, session_id: &str) -> Option<Arc<CommandRegistry>> {
        self.command_registry_for(session_id)
    }
}
