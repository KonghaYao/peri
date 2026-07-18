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
use crate::thread::CancelPolicy;

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
#[path = "mod_test.rs"]
mod tests;
