//! AgentGroup v2 — 会话级 Agent 管理与 Peer-to-Peer 管线
//!
//! AgentGroup 随 Session 创建，全生命周期存活。组内 Agent 平等，通过管线通讯。
//! **Agent 间全非阻塞**——创建子 Agent 后立即返回，子 Agent 独立执行 ReAct 循环。
//!
//! ## Cancel 策略
//!
//! - `Independent`：子 Agent 独立 Cancel Token，父取消不影响子
//! - `Cascade`：父取消级联取消全部子 Agent（通过 `CancellationToken::child_token()`）
//!
//! ## 事件聚合
//!
//! AgentGroup 收集组内全部 Agent 的事件，统一向外投递。
//! 外部只看到一个事件流，无需区分事件来自哪个 Agent。

pub mod pipeline;

pub use pipeline::{AgentId, AgentPipeline};

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::agent::events::ExecutorEvent;
use crate::session::queue::QueuedMessage;

// ─── CancelPolicy ─────────────────────────────────────────────────────────

/// Cancel 策略——创建 Agent 时指定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelPolicy {
    /// 子 Agent 独立 Cancel Token，父取消不影响子
    Independent,
    /// 父取消级联取消子 Agent（通过 child_token 关联）
    Cascade,
}

// ─── AgentHandle ──────────────────────────────────────────────────────────

/// Agent 实例句柄——包含 ID、名称、Cancel Token 等元数据
///
/// 消息收发通过 AgentPipeline 完成，AgentHandle 不持有 mailbox sender。
pub struct AgentHandle {
    /// Agent 唯一标识符
    pub agent_id: AgentId,
    /// 可选名称（用于调试和日志）
    pub name: Option<String>,
    /// Cancel Token——用于中断该 Agent 的 ReAct 循环
    pub cancel_token: Arc<CancellationToken>,
    /// Cancel 策略
    pub cancel_policy: CancelPolicy,
}

// ─── AgentGroup ───────────────────────────────────────────────────────────

/// 会话级 Agent 管理——Agent 创建/销毁、管线通讯、事件聚合
///
/// 随 Session 创建，全生命周期存活。内部维护：
/// - `agents`：已注册 Agent 的句柄（RwLock 保护）
/// - `pipeline`：Peer-to-Peer 消息管线
/// - `event_tx`：统一事件输出通道
pub struct AgentGroup {
    agents: RwLock<HashMap<AgentId, Arc<AgentHandle>>>,
    pipeline: AgentPipeline,
    event_tx: UnboundedSender<ExecutorEvent>,
}

impl AgentGroup {
    /// 创建 AgentGroup，返回 (AgentGroup, 事件接收端)
    pub fn new() -> (Self, UnboundedReceiver<ExecutorEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let group = Self {
            agents: RwLock::new(HashMap::new()),
            pipeline: AgentPipeline::new(),
            event_tx: tx,
        };
        (group, rx)
    }

    /// 注册新 Agent，返回 (AgentId, mailbox_rx, cancel_token)
    ///
    /// - `name`：可选名称，用于日志和调试
    /// - `cancel_policy`：Cancel 策略
    /// - `parent_token`：Cascade 模式下的父 Cancel Token（Independent 模式忽略）
    ///
    /// mailbox_rx 来自 `pipeline.register(id)`，发送通过 `pipeline.send/broadcast` 完成。
    pub fn register_agent(
        &self,
        name: Option<String>,
        cancel_policy: CancelPolicy,
        parent_token: Option<Arc<CancellationToken>>,
    ) -> (
        AgentId,
        UnboundedReceiver<QueuedMessage>,
        Arc<CancellationToken>,
    ) {
        let id = AgentId::new();
        let mailbox_rx = self.pipeline.register(id);

        // 根据 CancelPolicy 决定 Cancel Token 来源
        let cancel_token = match (cancel_policy, parent_token) {
            (CancelPolicy::Cascade, Some(parent)) => Arc::new(parent.child_token()),
            _ => Arc::new(CancellationToken::new()),
        };

        let handle = Arc::new(AgentHandle {
            agent_id: id,
            name,
            cancel_token: cancel_token.clone(),
            cancel_policy,
        });
        self.agents.write().insert(id, handle);

        (id, mailbox_rx, cancel_token)
    }

    /// 销毁 Agent——移除句柄并注销管线 mailbox
    pub fn destroy_agent(&self, id: AgentId) {
        // 先取消该 Agent（确保 ReAct 循环退出）
        if let Some(h) = self.agents.write().remove(&id) {
            h.cancel_token.cancel();
        }
        self.pipeline.unregister(id);
    }

    /// 列出所有已注册的 Agent ID
    pub fn list_agents(&self) -> Vec<AgentId> {
        self.agents.read().keys().copied().collect()
    }

    /// 获取指定 Agent 的句柄（只读引用）
    pub fn get_agent(&self, id: &AgentId) -> Option<Arc<AgentHandle>> {
        self.agents.read().get(id).cloned()
    }

    /// 向指定 Agent 发送消息（通过管线）
    pub fn send(&self, target: AgentId, msg: QueuedMessage) -> Result<(), anyhow::Error> {
        self.pipeline.send(target, msg)
    }

    /// 向所有已注册 Agent 广播消息（通过管线）
    pub fn broadcast(&self, msg: QueuedMessage) {
        self.pipeline.broadcast(msg);
    }

    /// 取消指定 Agent
    pub fn cancel_agent(&self, id: AgentId) {
        if let Some(h) = self.agents.read().get(&id) {
            h.cancel_token.cancel();
        }
    }

    /// 取消全部 Agent
    pub fn cancel_all(&self) {
        for h in self.agents.read().values() {
            h.cancel_token.cancel();
        }
    }

    /// 已注册 Agent 数量
    pub fn len(&self) -> usize {
        self.agents.read().len()
    }

    /// 是否无 Agent 注册
    pub fn is_empty(&self) -> bool {
        self.agents.read().is_empty()
    }

    /// 获取事件发送端的克隆（用于向外部投递事件）
    pub fn event_sender(&self) -> UnboundedSender<ExecutorEvent> {
        self.event_tx.clone()
    }
}

impl Default for AgentGroup {
    fn default() -> Self {
        Self::new().0
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::BaseMessage;
    use crate::session::queue::{MessageKind, MessageSource};

    fn make_queued(text: &str) -> QueuedMessage {
        QueuedMessage::new(
            MessageKind::Prompt,
            MessageSource::UserInput,
            BaseMessage::human(text.to_string()),
        )
    }

    #[test]
    fn test_agent_group_new() {
        let (group, _rx) = AgentGroup::new();
        assert!(group.is_empty());
    }

    #[test]
    fn test_register_agent_returns_triple() {
        let (group, _rx) = AgentGroup::new();

        let (id, _mailbox_rx, token) = group.register_agent(
            Some("test-agent".to_string()),
            CancelPolicy::Independent,
            None,
        );

        assert_eq!(group.len(), 1);
        assert!(!token.is_cancelled());
        let ids = group.list_agents();
        assert!(ids.contains(&id));
    }

    #[test]
    fn test_register_agent_independent_token() {
        let (group, _rx) = AgentGroup::new();
        let parent = Arc::new(CancellationToken::new());

        // Independent 策略：parent_token 被忽略
        let (_id, _rx, token) =
            group.register_agent(None, CancelPolicy::Independent, Some(parent.clone()));

        // 取消 parent 不影响子 Agent
        parent.cancel();
        assert!(
            !token.is_cancelled(),
            "Independent 模式下子 Agent 不应受父取消影响"
        );
    }

    #[test]
    fn test_register_agent_cascade_token() {
        let (group, _rx) = AgentGroup::new();
        let parent = Arc::new(CancellationToken::new());

        // Cascade 策略：parent 取消时子 Agent 级联取消
        let (_id, _rx, token) =
            group.register_agent(None, CancelPolicy::Cascade, Some(parent.clone()));

        parent.cancel();
        assert!(token.is_cancelled(), "Cascade 模式下父取消应级联到子 Agent");
    }

    #[test]
    fn test_register_agent_cascade_without_parent() {
        let (group, _rx) = AgentGroup::new();

        // Cascade 但无 parent_token，应回退到独立 token
        let (_id, _rx, token) = group.register_agent(None, CancelPolicy::Cascade, None);

        assert!(
            !token.is_cancelled(),
            "无 parent 时 Cascade 应创建独立 token"
        );
    }

    #[test]
    fn test_destroy_agent_removes_and_unregisters() {
        let (group, _rx) = AgentGroup::new();
        let (id, _mailbox_rx, token) = group.register_agent(None, CancelPolicy::Independent, None);

        assert_eq!(group.len(), 1);

        group.destroy_agent(id);

        assert!(group.is_empty());
        assert!(
            token.is_cancelled(),
            "destroy_agent 应取消该 Agent 的 token"
        );
    }

    #[test]
    fn test_send_to_registered_agent() {
        let (group, _rx) = AgentGroup::new();
        let (target_id, mut target_rx, _token) =
            group.register_agent(None, CancelPolicy::Independent, None);

        let msg = make_queued("hello from caller");
        assert!(group.send(target_id, msg).is_ok());

        let received = target_rx.try_recv().expect("目标 Agent 应收到消息");
        assert_eq!(received.message.content(), "hello from caller");
    }

    #[test]
    fn test_send_to_nonexistent_agent_fails() {
        let (group, _rx) = AgentGroup::new();
        let ghost = AgentId::new();

        let result = group.send(ghost, make_queued("hi"));
        assert!(result.is_err());
    }

    #[test]
    fn test_broadcast_to_all_agents() {
        let (group, _rx) = AgentGroup::new();

        let (_id_a, mut rx_a, _) =
            group.register_agent(Some("A".to_string()), CancelPolicy::Independent, None);
        let (_id_b, mut rx_b, _) =
            group.register_agent(Some("B".to_string()), CancelPolicy::Independent, None);

        group.broadcast(make_queued("announce"));

        let recv_a = rx_a.try_recv().expect("Agent A 应收到广播");
        let recv_b = rx_b.try_recv().expect("Agent B 应收到广播");
        assert_eq!(recv_a.message.content(), "announce");
        assert_eq!(recv_b.message.content(), "announce");
    }

    #[test]
    fn test_cancel_agent() {
        let (group, _rx) = AgentGroup::new();
        let (id, _mailbox_rx, token) = group.register_agent(None, CancelPolicy::Independent, None);

        assert!(!token.is_cancelled());
        group.cancel_agent(id);
        assert!(token.is_cancelled());
    }

    #[test]
    fn test_cancel_all() {
        let (group, _rx) = AgentGroup::new();

        let (_id_a, _rx_a, token_a) = group.register_agent(None, CancelPolicy::Independent, None);
        let (_id_b, _rx_b, token_b) = group.register_agent(None, CancelPolicy::Independent, None);

        group.cancel_all();

        assert!(token_a.is_cancelled());
        assert!(token_b.is_cancelled());
    }

    #[test]
    fn test_get_agent() {
        let (group, _rx) = AgentGroup::new();
        let (id, _rx, _token) =
            group.register_agent(Some("finder".to_string()), CancelPolicy::Independent, None);

        let handle = group.get_agent(&id).expect("应找到已注册 Agent");
        assert_eq!(handle.agent_id, id);
        assert_eq!(handle.name.as_deref(), Some("finder"));
        assert_eq!(handle.cancel_policy, CancelPolicy::Independent);
    }

    #[test]
    fn test_get_agent_nonexistent() {
        let (group, _rx) = AgentGroup::new();
        let ghost = AgentId::new();
        assert!(group.get_agent(&ghost).is_none());
    }

    #[test]
    fn test_event_sender_clone() {
        let (group, _rx) = AgentGroup::new();
        let sender = group.event_sender();
        // 不 panic 即可——验证 clone 可用
        let _sender2 = sender.clone();
    }

    #[test]
    fn test_send_via_pipeline_after_destroy_fails() {
        let (group, _rx) = AgentGroup::new();
        let (id, _mailbox_rx, _token) = group.register_agent(None, CancelPolicy::Independent, None);

        group.destroy_agent(id);

        let result = group.send(id, make_queued("late"));
        assert!(result.is_err(), "destroy 后发送应失败");
    }

    #[test]
    fn test_multiple_agents_independent_lifecycle() {
        let (group, _rx) = AgentGroup::new();

        let (id_a, mut rx_a, token_a) =
            group.register_agent(Some("A".to_string()), CancelPolicy::Independent, None);
        let (id_b, mut rx_b, token_b) =
            group.register_agent(Some("B".to_string()), CancelPolicy::Independent, None);

        assert_eq!(group.len(), 2);

        // A 取消不影响 B
        group.cancel_agent(id_a);
        assert!(token_a.is_cancelled());
        assert!(!token_b.is_cancelled());

        // A 的 mailbox 已关闭，B 的仍可用
        let _ = rx_a.try_recv(); // 可能收到 None（channel closed）
        group.send(id_b, make_queued("still alive")).ok();
        assert!(rx_b.try_recv().is_ok(), "B 的 mailbox 应仍可用");
    }
}
