//! Agent 间 Peer-to-Peer 管线
//!
//! 每个注册的 Agent 获得一个独立 mailbox（`UnboundedReceiver<QueuedMessage>`），
//! 其他 Agent 通过 `pipeline.send(target, msg)` 或 `pipeline.broadcast(msg)` 投递消息。
//! 内部用 `HashMap<AgentId, UnboundedSender>` 管理，`parking_lot::Mutex` 保证线程安全。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use uuid::Uuid;

use crate::session::queue::QueuedMessage;

// ─── AgentId ───────────────────────────────────────────────────────────────

/// Agent 唯一标识符 — UUID v7（时间有序，跨进程安全）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(Uuid);

impl AgentId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ─── AgentPipeline ────────────────────────────────────────────────────────

/// Agent 间 Peer-to-Peer 管线
///
/// 每个 Agent 通过 `register(id)` 获取一个独立的 `UnboundedReceiver<QueuedMessage>`，
/// 其他 Agent 通过 `send(target, msg)` 或 `broadcast(msg)` 向目标投递消息。
#[derive(Clone)]
pub struct AgentPipeline {
    mailboxes: Arc<Mutex<HashMap<AgentId, UnboundedSender<QueuedMessage>>>>,
}

impl AgentPipeline {
    /// 创建空管线
    pub fn new() -> Self {
        Self {
            mailboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 注册 Agent，返回该 Agent 的 mailbox 接收端
    ///
    /// 调用者持有返回的 `UnboundedReceiver`，在 ReAct 循环中 `recv` 消费消息。
    pub fn register(&self, id: AgentId) -> UnboundedReceiver<QueuedMessage> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.mailboxes.lock().insert(id, tx);
        rx
    }

    /// 注销 Agent，移除 mailbox
    ///
    /// 已注册的 receiver 会在 sender drop 后收到 `None`。
    pub fn unregister(&self, id: AgentId) {
        self.mailboxes.lock().remove(&id);
    }

    /// 向指定 Agent 发送消息
    ///
    /// 目标 Agent 不存在时返回错误。
    pub fn send(&self, target: AgentId, msg: QueuedMessage) -> Result<(), anyhow::Error> {
        let mailboxes = self.mailboxes.lock();
        if let Some(tx) = mailboxes.get(&target) {
            tx.send(msg)
                .map_err(|e| anyhow::anyhow!("mailbox closed: {e}"))
        } else {
            Err(anyhow::anyhow!("agent {target:?} not found"))
        }
    }

    /// 向所有已注册 Agent 广播消息
    ///
    /// 忽略发送失败的 mailbox（已关闭或缓冲区满）。
    pub fn broadcast(&self, msg: QueuedMessage) {
        let mailboxes = self.mailboxes.lock();
        for tx in mailboxes.values() {
            // 广播不关心单个 mailbox 状态，忽略错误
            let _ = tx.send(msg.clone());
        }
    }

    /// 列出所有已注册的 Agent ID
    pub fn list(&self) -> Vec<AgentId> {
        self.mailboxes.lock().keys().copied().collect()
    }

    /// 已注册 Agent 数量
    pub fn len(&self) -> usize {
        self.mailboxes.lock().len()
    }

    /// 是否无 Agent 注册
    pub fn is_empty(&self) -> bool {
        self.mailboxes.lock().is_empty()
    }
}

impl Default for AgentPipeline {
    fn default() -> Self {
        Self::new()
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
    fn test_agent_id_unique() {
        let id1 = AgentId::new();
        let id2 = AgentId::new();
        assert_ne!(id1, id2, "每次 new() 应生成不同 ID");
        assert_ne!(id1.as_uuid(), uuid::Uuid::nil());
    }

    #[test]
    fn test_agent_id_default() {
        let id = AgentId::default();
        assert_ne!(id.as_uuid(), uuid::Uuid::nil());
    }

    #[test]
    fn test_register_and_list() {
        let pipeline = AgentPipeline::new();
        assert!(pipeline.is_empty());

        let id = AgentId::new();
        let _rx = pipeline.register(id);

        assert_eq!(pipeline.len(), 1);
        let ids = pipeline.list();
        assert!(ids.contains(&id));
    }

    #[test]
    fn test_unregister_removes_mailbox() {
        let pipeline = AgentPipeline::new();
        let id = AgentId::new();
        let _rx = pipeline.register(id);

        pipeline.unregister(id);
        assert!(pipeline.is_empty());
    }

    #[test]
    fn test_send_to_registered_agent() {
        let pipeline = AgentPipeline::new();
        let id = AgentId::new();
        let mut rx = pipeline.register(id);

        let msg = make_queued("hello");
        assert!(pipeline.send(id, msg).is_ok());

        let received = rx.try_recv().expect("应收到消息");
        assert_eq!(received.message.content(), "hello");
    }

    #[test]
    fn test_send_to_unregistered_agent_fails() {
        let pipeline = AgentPipeline::new();
        let ghost = AgentId::new();
        let result = pipeline.send(ghost, make_queued("hi"));

        assert!(result.is_err(), "未注册 Agent 应返回错误");
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_broadcast_reaches_all() {
        let pipeline = AgentPipeline::new();

        let id_a = AgentId::new();
        let id_b = AgentId::new();
        let mut rx_a = pipeline.register(id_a);
        let mut rx_b = pipeline.register(id_b);

        pipeline.broadcast(make_queued("announce"));

        let recv_a = rx_a.try_recv().expect("Agent A 应收到广播");
        let recv_b = rx_b.try_recv().expect("Agent B 应收到广播");
        assert_eq!(recv_a.message.content(), "announce");
        assert_eq!(recv_b.message.content(), "announce");
    }

    #[test]
    fn test_broadcast_skips_dropped_mailbox() {
        let pipeline = AgentPipeline::new();

        let id_a = AgentId::new();
        let id_b = AgentId::new();
        let mut rx_a = pipeline.register(id_a);
        let _rx_b = pipeline.register(id_b);

        // 注销 id_b 后广播不应 panic
        pipeline.unregister(id_b);
        pipeline.broadcast(make_queued("partial"));

        let recv_a = rx_a.try_recv().expect("Agent A 应收到广播");
        assert_eq!(recv_a.message.content(), "partial");
    }

    #[test]
    fn test_send_after_unregister_fails() {
        let pipeline = AgentPipeline::new();
        let id = AgentId::new();
        let _rx = pipeline.register(id);

        pipeline.unregister(id);
        let result = pipeline.send(id, make_queued("late"));
        assert!(result.is_err());
    }
}
