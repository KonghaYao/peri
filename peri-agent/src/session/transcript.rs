//! MessageTranscript v2 — 会话消息权威存储
//!
//! Transcript 是会话全部消息的唯一真相源。核心特性：
//! - **MessageId 寻址**：内部维护 `HashMap<MessageId, usize>` 索引表，O(1) 查找
//! - **只追加优先**：正常 ReAct 循环中消息仅尾部追加，禁止 prepend/中间插入
//! - **Staging 两阶段写入**：AI 消息 + ToolResult 原子提交
//! - **标记代替删除**：Compact 先以 `truncated` / `excluded` 标记表达失效，消息本体不变；
//!   Full Compact 在 store 确认提交后才释放被排除的 own 条目（见 `shrink_after_full_commit`）
//! - **异步持久化**：append 后通过 unbounded_channel 异步触发 ThreadStore 写入
//!
//! 模块结构：条目与暂存模型见 `entry`，持久化接线（前台）见 `persistence_port`，
//! 后台 writer 见 `persistence`；`MessageTranscript` 的内存模型、标记与压缩生命周期留在本模块。

mod message_transcript;

#[path = "transcript_view.rs"]
mod view;

#[path = "transcript/rewind.rs"]
mod rewind;

pub use message_transcript::DEFAULT_PENDING_MAX_ITEMS;
pub use view::TranscriptRevision;

pub use message_transcript::DEFAULT_PENDING_MAX_BYTES;

pub use message_transcript::CompactionCommitState;

pub use message_transcript::PersistOp;

mod atomic_batch;
pub use atomic_batch::AtomicBatchCommit;
mod entry;
mod persistence;
mod persistence_port;
pub(crate) use persistence_port::FlushError;

pub use entry::{StagedData, TranscriptEntry};
pub use message_transcript::MessageTranscript;

pub use message_transcript::IdempotentReminderPort;

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "transcript_test.rs"]
mod tests;

#[cfg(test)]
#[path = "transcript_projection_test.rs"]
mod projection_tests;
