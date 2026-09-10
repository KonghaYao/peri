//! Session 级 Cron continuation 与 MCP subscription 接线。

use super::cron_bridge::SessionCronBridge;
use super::SessionManager;

impl SessionManager {
    /// Host 启动时绑定 cron continuation 入口。既有 bridge 会在下一次 lazy 检查前
    /// 尚未创建，因此只允许首次写入。
    pub fn bind_cron_continuation(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<peri_acp_types::cron::CronContinuationRequest>,
    ) {
        let mut slot = self.inner.cron_continuation_tx.lock();
        if slot.is_none() {
            *slot = Some(tx);
        }
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
        let continuation_tx = match self.inner.cron_continuation_tx.lock().clone() {
            Some(tx) => tx,
            None => return false,
        };
        // Fast path
        if let Some(session) = self.inner.sessions.get(session_id) {
            if session.cron_bridge.is_some() {
                return true;
            }
        }
        // Slow path: bind a session-scoped scheduler subscription to Host continuation.
        if self.session_inbox_for(session_id).is_none() {
            return false;
        }
        if let Some(mut session) = self.inner.sessions.get_mut(session_id) {
            if session.cron_bridge.is_none() {
                session.cron_bridge = Some(SessionCronBridge::start(
                    session_id.to_string(),
                    &scheduler,
                    continuation_tx,
                ));
                tracing::info!(session_id = %session_id, "session cron bridge started");
            }
        }
        // 理论性竞态：session_inbox_for 成功后 get_mut 返回 None（并发
        // close_session 恰好移除）时返回 true 但未实际创建——生产调用方忽略返回值。
        true
    }

    /// 确保指定 session 的 MCP 订阅 inbox 已注册（lazy-init，幂等）。
    ///
    /// 首次调用：把 session 级 inbox handle 注册到 `McpSubscriptionPort`
    /// （peri-middlewares 实现侧维护 session_id → inbox 注册表）。此后每
    /// turn 重复调用均为 no-op（HashMap insert 幂等）。注册跨 turn 存活，
    /// close_session 时经 [`SessionManager::close_session`] 注销。
    ///
    /// session 不存在或端口未配置时返回 false。
    pub fn mcp_subscription_for(&self, session_id: &str) -> bool {
        let port = match &self.inner.mcp_subscription {
            Some(p) => p.clone(),
            None => return false,
        };
        let Some(inbox) = self.session_inbox_for(session_id) else {
            return false;
        };
        port.register_inbox(session_id, inbox.handle());
        tracing::trace!(session_id = %session_id, "MCP 订阅 inbox 已注册");
        true
    }
}
