//! Thread 元数据契约类型（自 peri-agent/src/thread 下沉，接口契约归 peri-acp-types）。
//!
//! 纯数据 + 强类型枚举，供 ThreadStore 契约（`crate::store`）与各层引用。
//! peri-agent::thread 保留 re-export 兼容路径。

mod types;

pub use types::{AgentStatus, CancelPolicy, ThreadId, ThreadMeta, ThreadMetaParseError};
