use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{
    messages::BaseMessage,
    thread::{ThreadId, ThreadStore},
};

/// State trait - 所有 Agent 状态必须实现此 trait
/// 与 TypeScript BaseAgentStateType 对齐
pub trait State: Send + Sync + Clone + 'static {
    fn cwd(&self) -> &str;
    fn set_cwd(&mut self, cwd: impl Into<String>);
    fn messages(&self) -> &[BaseMessage];
    fn add_message(&mut self, message: BaseMessage);

    /// 将消息前插到消息历史开头（系统消息置于最前）
    fn prepend_message(&mut self, message: BaseMessage);

    fn current_step(&self) -> usize;
    fn set_current_step(&mut self, step: usize);

    fn get_context(&self, key: &str) -> Option<&str>;
    fn set_context(&mut self, key: impl Into<String>, value: impl Into<String>);

    fn token_tracker(&self) -> &crate::agent::token::TokenTracker;
    fn token_tracker_mut(&mut self) -> &mut crate::agent::token::TokenTracker;

    /// 获取消息的可变引用（用于 micro_compact 等原地修改场景）
    fn messages_mut(&mut self) -> &mut Vec<BaseMessage>;

    /// Push a recall item into the session's recall buffer.
    fn push_recall(&mut self, item: String);

    /// Drain all recall items (one-time consumption).
    fn drain_recall(&mut self) -> Vec<String>;

    /// messages[..ancestor_len] = 只读祖先消息（compact 边界）
    fn ancestor_len(&self) -> usize {
        0
    }

    /// 持久化后端（compact 后 invalidate cache 用）
    fn store(&self) -> Option<&Arc<dyn ThreadStore>> {
        None
    }

    /// 持久化目标 thread id（compact 后 invalidate cache 用）
    fn own_thread_id(&self) -> Option<&ThreadId> {
        None
    }
}

/// 基础 Agent 状态（与 TypeScript BaseAgentStateType 对齐）
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AgentState {
    pub cwd: String,
    #[serde(skip)]
    pub messages: Vec<BaseMessage>,
    pub current_step: usize,
    pub context: HashMap<String, String>,
    pub token_tracker: crate::agent::token::TokenTracker,
    /// 可选持久化后端（绑定后 add_message 自动写入）
    #[serde(skip)]
    store: Option<Arc<dyn ThreadStore>>,
    /// 持久化目标 thread id
    #[serde(skip)]
    thread_id: Option<ThreadId>,
    /// 有序持久化通道：保证消息按 add_message 调用顺序写入 SQLite，
    /// 避免 tokio::spawn 的 fire-and-forget 模式因 .await 让步导致乱序。
    #[serde(skip)]
    persist_tx: Option<Arc<tokio::sync::mpsc::UnboundedSender<BaseMessage>>>,
    /// 会话级 recall 缓冲区：收集运行时事件通知，executor 在构建用户消息前 drain 消费。
    /// 不随 session 持久化，仅存活于当前会话生命周期内。
    #[serde(skip)]
    recall_buffer: Vec<String>,
    /// messages[..ancestor_len] = 只读祖先消息（compact 边界标记）
    #[serde(skip)]
    ancestor_len: usize,
}

impl std::fmt::Debug for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentState")
            .field("cwd", &self.cwd)
            .field("messages", &self.messages)
            .field("current_step", &self.current_step)
            .field("context", &self.context)
            .field("store", &self.store.as_ref().map(|_| "ThreadStore"))
            .field("thread_id", &self.thread_id)
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

    /// 绑定持久化后端，之后每次 add_message 自动写入
    ///
    /// 使用有序通道 + 专用 writer 任务替代 fire-and-forget tokio::spawn，
    /// 保证消息按 add_message 调用顺序写入 SQLite，
    /// 避免 spawn 任务的 .await 让步导致 rowid 乱序（#history-restore-bug）。
    pub fn with_persistence(
        mut self,
        store: Arc<dyn ThreadStore>,
        thread_id: impl Into<String>,
    ) -> Self {
        self.store = Some(store.clone());
        self.thread_id = Some(thread_id.into());
        // [TRAP] 使用 unbounded channel 是有意为之：保证消息按 push 顺序写入 SQLite，
        // 规避 rowid 与并发写入的顺序歧义。代价是 SQLite append 慢时（磁盘压力 / FSYNC
        // 争用 / 长会话）会在内存中积压 BaseMessage clone。
        // (CS#2 架构审查已确认：典型会话可接受；此处加 counter + 周期性 warn! 提升可观测性)
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BaseMessage>();
        self.persist_tx = Some(Arc::new(tx));
        let tid = self.thread_id.clone().unwrap();
        tokio::spawn(async move {
            let mut processed: u64 = 0;
            let mut last_warn_at: u64 = 0;
            while let Some(msg) = rx.recv().await {
                if let Err(e) = store.append_message(&tid, msg).await {
                    tracing::warn!("ordered persist failed: {e}");
                }
                processed = processed.saturating_add(1);
                // 每 1000 条记录一次进度；以幂等间距输出避免日志噪声
                let bucket = processed / 1000;
                if bucket > last_warn_at {
                    last_warn_at = bucket;
                    tracing::trace!(
                        thread_id = %tid,
                        processed,
                        "persist writer: 已写入 {processed} 条消息"
                    );
                }
            }
        });
        self
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

    /// 使用 ThreadStore 的 load_context 构建完整上下文（含祖先快照）
    pub async fn with_thread_context(
        thread_id: ThreadId,
        store: Arc<dyn ThreadStore>,
    ) -> anyhow::Result<Self> {
        let meta = store.load_meta(&thread_id).await?;
        let all_messages = store.load_context(&thread_id).await?;
        let own_messages = store.load_messages(&thread_id).await?;
        let ancestor_len = all_messages.len().saturating_sub(own_messages.len());
        Ok(Self::new(&meta.cwd)
            .with_messages_from(all_messages)
            .with_ancestor_len(ancestor_len)
            .with_persistence(store, thread_id))
    }

    /// 从已有消息列表填充（内部辅助）
    fn with_messages_from(mut self, messages: Vec<BaseMessage>) -> Self {
        self.messages = messages;
        self
    }

    /// messages[..ancestor_len] = 只读祖先消息
    pub fn ancestor_len(&self) -> usize {
        self.ancestor_len
    }

    pub fn with_ancestor_len(mut self, len: usize) -> Self {
        self.ancestor_len = len;
        self
    }

    pub fn store(&self) -> Option<&Arc<dyn ThreadStore>> {
        self.store.as_ref()
    }

    pub fn own_thread_id(&self) -> Option<&ThreadId> {
        self.thread_id.as_ref()
    }
}

impl State for AgentState {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn set_cwd(&mut self, cwd: impl Into<String>) {
        self.cwd = cwd.into();
    }

    fn messages(&self) -> &[BaseMessage] {
        &self.messages
    }

    fn add_message(&mut self, message: BaseMessage) {
        // 有序持久化：通过通道发送到专用 writer 任务，保证写入顺序
        if let Some(ref tx) = self.persist_tx {
            if let Err(e) = tx.send(message.clone()) {
                tracing::warn!("ordered persist send failed (channel closed): {e}");
            }
        }
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

    fn prepend_message(&mut self, message: BaseMessage) {
        self.messages.insert(0, message);
    }

    fn current_step(&self) -> usize {
        self.current_step
    }

    fn set_current_step(&mut self, step: usize) {
        self.current_step = step;
    }

    fn get_context(&self, key: &str) -> Option<&str> {
        self.context.get(key).map(|s| s.as_str())
    }

    fn set_context(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.context.insert(key.into(), value.into());
    }

    fn token_tracker(&self) -> &crate::agent::token::TokenTracker {
        &self.token_tracker
    }
    fn token_tracker_mut(&mut self) -> &mut crate::agent::token::TokenTracker {
        &mut self.token_tracker
    }

    fn messages_mut(&mut self) -> &mut Vec<BaseMessage> {
        &mut self.messages
    }

    fn push_recall(&mut self, item: String) {
        self.recall_buffer.push(item);
    }

    fn drain_recall(&mut self) -> Vec<String> {
        std::mem::take(&mut self.recall_buffer)
    }

    fn ancestor_len(&self) -> usize {
        self.ancestor_len
    }

    fn store(&self) -> Option<&Arc<dyn ThreadStore>> {
        self.store.as_ref()
    }

    fn own_thread_id(&self) -> Option<&ThreadId> {
        self.thread_id.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("state_test.rs");
}
