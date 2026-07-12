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
#[path = "pipeline_test.rs"]
mod tests;
