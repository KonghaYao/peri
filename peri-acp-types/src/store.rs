//! ThreadStore 契约 — 持久化存储抽象（自 peri-agent/src/thread 下沉）。
//!
//! 接口契约归 peri-acp-types：`SqliteThreadStore`（peri-resources）实现本 trait，
//! Agent/ACP/TUI 经本 trait 引用存储，不直接实例化。

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::messages::{BaseMessage, MessageId};
use crate::projection::MessageProjectionDirective;
use crate::thread::{ThreadId, ThreadMeta};

#[derive(Clone, Debug)]
pub struct CompactionLifecycle {
    pub flag_updates: Vec<(MessageId, MessageFlags)>,
    pub appended_messages: Vec<BaseMessage>,
}

// ─── MessageFlags ─────────────────────────────────────────────────────────────

/// 消息标记 — Compact 用，标记代替删除
///
/// - `truncated`：Micro compact 标记，LLM 请求时截断该消息输出
/// - `excluded`：Full / Smart compact 标记，LLM 请求时跳过该消息
/// - `projection`：投影指令（v2）。None 表示旧版 flag 或未 compact。
///   旧 JSON（无此字段）反序列化后为 None。
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFlags {
    pub truncated: bool,
    pub excluded: bool,
    /// 投影指令（v2）。None 表示旧版 flag 或未 compact。
    /// 旧 JSON（无此字段）反序列化后为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<MessageProjectionDirective>,
}

#[async_trait]
pub trait ThreadStore: Send + Sync {
    /// 创建新 thread，返回分配的 ThreadId
    async fn create_thread(&self, meta: ThreadMeta) -> Result<ThreadId>;

    /// 追加消息到指定 thread（追加写，不覆盖）
    async fn append_messages(&self, id: &ThreadId, msgs: &[BaseMessage]) -> Result<()>;

    /// 追加单条消息到指定 thread（默认实现复用 append_messages）
    async fn append_message(&self, id: &ThreadId, message: BaseMessage) -> Result<()> {
        self.append_messages(id, &[message]).await
    }

    /// 加载指定 thread 的全部消息
    async fn load_messages(&self, id: &ThreadId) -> Result<Vec<BaseMessage>>;

    /// 加载指定 thread 的元数据
    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta>;

    /// 更新指定 thread 的元数据
    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()>;

    /// 列举所有 thread 元数据，按 updated_at 降序（不含 hidden 的子 agent）
    async fn list_threads(&self) -> Result<Vec<ThreadMeta>>;

    /// 删除指定 thread（包含消息和元数据）
    async fn delete_thread(&self, id: &ThreadId) -> Result<()>;

    /// 更新指定 thread 的标题
    async fn update_title(&self, id: &ThreadId, title: &str) -> Result<()> {
        let mut meta = self.load_meta(id).await?;
        meta.title = Some(title.to_string());
        self.update_meta(id, meta).await
    }

    /// 加载 thread 的完整上下文（含祖先链 + 缓存）
    async fn load_context(&self, thread_id: &ThreadId) -> Result<Vec<BaseMessage>>;

    /// 列举指定父 thread 的直接子 thread
    async fn list_child_threads(&self, parent_id: &ThreadId) -> Result<Vec<ThreadMeta>>;

    /// 递归列举以 root_id 为根的所有 thread（含自身）
    async fn list_session_threads(&self, root_id: &ThreadId) -> Result<Vec<ThreadMeta>>;

    /// 更新 thread 的 agent_status 字段
    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> Result<()>;

    /// 清除 thread 的 cached_context
    async fn invalidate_context_cache(&self, thread_id: &ThreadId) -> Result<()>;

    /// 按 message_id 列表精确删除消息，并刷新 cached_context。
    async fn delete_messages(&self, thread_id: &ThreadId, message_ids: &[MessageId]) -> Result<()>;

    /// 更新消息的 compact 标记（truncated / excluded / projection directive）
    async fn update_message_flags(
        &self,
        message_id: &MessageId,
        flags: &MessageFlags,
    ) -> Result<()> {
        let _ = (message_id, flags);
        Ok(()) // 默认 no-op
    }

    /// 返回后端是否支持原子 compact lifecycle 提交。
    fn supports_compaction_lifecycle(&self) -> bool {
        false
    }

    /// 原子持久化压缩生命周期的消息标记和追加消息。
    async fn commit_compaction_lifecycle(
        &self,
        thread_id: &ThreadId,
        lifecycle: &CompactionLifecycle,
    ) -> Result<()> {
        let _ = (thread_id, lifecycle);
        anyhow::bail!("unsupported compact lifecycle persistence")
    }

    /// 加载 thread 中所有非默认 compact 标记
    async fn load_message_flags(
        &self,
        _thread_id: &ThreadId,
    ) -> Result<HashMap<MessageId, MessageFlags>> {
        Ok(HashMap::new())
    }

    /// 删除指定消息之后的所有记录（用于 rewind）
    ///
    /// 查找 message_id 对应的序列位置，删除该位置之后的所有消息。
    /// 若 message_id 不存在则不执行任何操作。
    async fn delete_messages_since(
        &self,
        thread_id: &ThreadId,
        message_id: &MessageId,
    ) -> Result<()> {
        let _ = (thread_id, message_id);
        Ok(()) // 默认 no-op
    }

    /// H6: 获取 context cache epoch 值。
    ///
    /// 每次 compact 提交后递增，用于检测 context_cache 是否因 compact 变更而失效。
    async fn get_context_cache_epoch(&self, _thread_id: &ThreadId) -> Result<u64> {
        Ok(0) // 默认无 epoch 支持
    }
}
