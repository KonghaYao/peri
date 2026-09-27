use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{messages::BaseMessage, session::MessageQueue};

/// 基础 Agent 状态（与 TypeScript BaseAgentStateType 对齐）
///
/// middleware_runner 通过 `MiddlewareState` trait 桥接 v2 stages ↔ 钩子，
/// `AgentState` 直接 impl `MiddlewareState`（不再经过 `State` trait 中间层）。
///
/// 持久化不经本类型：会话历史的事实源是 `MessageTranscript`（绑定的是
/// `SessionResources` 门面，见 `session/transcript.rs`），本类型只承载一轮执行内
/// 的可见状态。
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AgentState {
    pub cwd: String,
    #[serde(skip)]
    pub messages: Vec<BaseMessage>,
    pub current_step: usize,
    pub context: HashMap<String, String>,
    pub token_tracker: crate::agent::token::TokenTracker,
    /// 会话级 recall 缓冲区：收集运行时事件通知，executor 在构建用户消息前 drain 消费。
    /// 不随 session 持久化，仅存活于当前会话生命周期内。
    #[serde(skip)]
    recall_buffer: Vec<String>,
    /// v2 MessageQueue 句柄——middleware（goal steering / stop-hook feedback）
    /// 通过它向 session 级共享收件箱 push 异步消息。
    ///
    /// **共享语义**：`MessageQueue` 内部用 `Arc<Mutex<VecDeque>> + Arc<Notify>`，
    /// clone 共享底层数据。`AgentState::new` 或 `AgentContext::from_stage` 构造时
    /// 传入 `ctx.session.queue.clone()`，因此 middleware push 的消息直接进入 v2 queue，
    /// Receive / End 阶段统一消费。
    #[serde(skip)]
    pub v2_queue: MessageQueue,
}

impl std::fmt::Debug for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentState")
            .field("cwd", &self.cwd)
            .field("messages", &self.messages)
            .field("current_step", &self.current_step)
            .field("context", &self.context)
            .field("token_tracker", &self.token_tracker)
            .finish()
    }
}

impl AgentState {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Default::default()
        }
    }

    /// 从已有消息历史构建（用于多轮对话续接）
    pub fn with_messages(cwd: impl Into<String>, messages: Vec<BaseMessage>) -> Self {
        Self {
            cwd: cwd.into(),
            messages,
            ..Default::default()
        }
    }

    /// 消费 state，返回消息历史（用于传回调用方保存）
    pub fn into_messages(self) -> Vec<BaseMessage> {
        self.messages
    }

    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.insert(key.into(), value.into());
        self
    }

    pub fn get_context(&self, key: &str) -> Option<&str> {
        self.context.get(key).map(|s| s.as_str())
    }

    pub fn set_context(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.context.insert(key.into(), value.into());
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn set_cwd(&mut self, cwd: impl Into<String>) {
        self.cwd = cwd.into();
    }

    pub fn messages(&self) -> &[BaseMessage] {
        &self.messages
    }

    pub fn add_message(&mut self, message: BaseMessage) {
        self.messages.push(message);
        // 消息数量超过阈值时发出警告，提示使用 /compact 压缩上下文以降低内存占用
        let count = self.messages.len();
        if count > 100 && count.is_multiple_of(100) {
            tracing::warn!(
                count,
                "AgentState: message history is large ({} messages); consider using /compact to reduce memory usage",
                count
            );
        }
    }

    pub fn prepend_message(&mut self, message: BaseMessage) {
        self.messages.insert(0, message);
    }

    pub fn messages_mut(&mut self) -> &mut Vec<BaseMessage> {
        &mut self.messages
    }

    pub fn current_step(&self) -> usize {
        self.current_step
    }

    pub fn set_current_step(&mut self, step: usize) {
        self.current_step = step;
    }

    pub fn token_tracker(&self) -> &crate::agent::token::TokenTracker {
        &self.token_tracker
    }

    pub fn token_tracker_mut(&mut self) -> &mut crate::agent::token::TokenTracker {
        &mut self.token_tracker
    }

    pub fn push_recall(&mut self, item: String) {
        self.recall_buffer.push(item);
    }

    pub fn drain_recall(&mut self) -> Vec<String> {
        std::mem::take(&mut self.recall_buffer)
    }

    /// v2 MessageQueue 句柄（共享 session 级实例）
    pub fn v2_queue(&self) -> &MessageQueue {
        &self.v2_queue
    }
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
