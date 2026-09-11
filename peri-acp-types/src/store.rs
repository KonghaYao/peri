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
use crate::system_reminder::{
    decode_system_reminder_json, SystemReminder, TrustedSystemReminder,
    TrustedSystemReminderFactory,
};
use crate::thread::{ThreadId, ThreadListEntry, ThreadMeta};

/// Current inline history envelope version.
pub const PERSISTED_PAYLOAD_VERSION: u16 = 1;

/// A single logical history record. Canonical reminders are never stored as user messages.
#[derive(Clone, Debug)]
pub enum PersistedPayload {
    Message(BaseMessage),
    SystemReminder {
        id: MessageId,
        reminder: TrustedSystemReminder,
    },
}

impl PersistedPayload {
    pub fn id(&self) -> MessageId {
        match self {
            Self::Message(message) => message.id(),
            Self::SystemReminder { id, .. } => *id,
        }
    }

    pub fn as_message(&self) -> Option<&BaseMessage> {
        match self {
            Self::Message(message) => Some(message),
            Self::SystemReminder { .. } => None,
        }
    }
}

#[derive(Serialize)]
struct PersistedEnvelopeRef<'a> {
    version: u16,
    #[serde(flatten)]
    payload: PersistedPayloadRef<'a>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PersistedPayloadRef<'a> {
    Message {
        message: &'a BaseMessage,
    },
    SystemReminder {
        id: MessageId,
        reminder: &'a SystemReminder,
    },
}

#[derive(Deserialize)]
struct RawPersistedEnvelope {
    version: u16,
    #[serde(flatten)]
    payload: RawPersistedPayload,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawPersistedPayload {
    Message {
        message: BaseMessage,
    },
    SystemReminder {
        id: MessageId,
        reminder: serde_json::Value,
    },
}

pub fn serialize_persisted_payload(payload: &PersistedPayload) -> Result<String> {
    let payload = match payload {
        PersistedPayload::Message(message) => PersistedPayloadRef::Message { message },
        PersistedPayload::SystemReminder { id, reminder } => PersistedPayloadRef::SystemReminder {
            id: *id,
            reminder: reminder.as_reminder(),
        },
    };
    Ok(serde_json::to_string(&PersistedEnvelopeRef {
        version: PERSISTED_PAYLOAD_VERSION,
        payload,
    })?)
}

/// Reads the V1 envelope or an unwrapped legacy `BaseMessage` row.
/// Unknown/corrupt envelopes return an error and can never establish recovery trust.
pub fn deserialize_persisted_payload(input: &str) -> Result<PersistedPayload> {
    let value: serde_json::Value = serde_json::from_str(input)?;
    if value.get("version").is_none() && value.get("type").is_none() {
        return Ok(PersistedPayload::Message(serde_json::from_value(value)?));
    }
    let envelope: RawPersistedEnvelope = serde_json::from_value(value)?;
    if envelope.version != PERSISTED_PAYLOAD_VERSION {
        anyhow::bail!("unsupported persisted payload version {}", envelope.version);
    }
    match envelope.payload {
        RawPersistedPayload::Message { message } => Ok(PersistedPayload::Message(message)),
        RawPersistedPayload::SystemReminder { id, reminder } => {
            let reminder = serde_json::to_vec(&reminder)?;
            let reminder = decode_system_reminder_json(&reminder)?;
            Ok(PersistedPayload::SystemReminder {
                id,
                reminder: TrustedSystemReminderFactory::for_recovery().construct(reminder)?,
            })
        }
    }
}

/// Frozen, read-only context inherited by a child thread, including projection state.
#[derive(Clone, Debug, Default)]
pub struct InheritedContext {
    pub payloads: Vec<PersistedPayload>,
    pub flags: HashMap<MessageId, MessageFlags>,
}

#[derive(Serialize, Deserialize)]
struct InheritedContextEnvelope {
    version: u16,
    payloads: Vec<String>,
    flags: HashMap<MessageId, MessageFlags>,
}

impl InheritedContext {
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(&InheritedContextEnvelope {
            version: 1,
            payloads: self
                .payloads
                .iter()
                .map(serialize_persisted_payload)
                .collect::<Result<_>>()?,
            flags: self.flags.clone(),
        })?)
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let envelope: InheritedContextEnvelope = serde_json::from_str(json)?;
        if envelope.version != 1 {
            anyhow::bail!("unsupported inherited context version {}", envelope.version);
        }
        let payloads = envelope
            .payloads
            .iter()
            .map(|payload| deserialize_persisted_payload(payload))
            .collect::<Result<Vec<_>>>()?;
        let ids = payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<std::collections::HashSet<_>>();
        if ids.len() != payloads.len() || envelope.flags.keys().any(|id| !ids.contains(id)) {
            anyhow::bail!("invalid inherited context message references");
        }
        Ok(Self {
            payloads,
            flags: envelope.flags,
        })
    }
}

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

    /// 追加逻辑 payload；默认实现仅支持普通消息，供 legacy 测试替身兼容。
    async fn append_payloads(&self, id: &ThreadId, payloads: &[PersistedPayload]) -> Result<()> {
        let messages = payloads
            .iter()
            .map(|payload| {
                payload
                    .as_message()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("store does not support persisted reminders"))
            })
            .collect::<Result<Vec<_>>>()?;
        self.append_messages(id, &messages).await
    }

    /// 加载逻辑 payload；默认将 legacy 普通消息提升为 envelope 变体。
    async fn load_payloads(&self, id: &ThreadId) -> Result<Vec<PersistedPayload>> {
        Ok(self
            .load_messages(id)
            .await?
            .into_iter()
            .map(PersistedPayload::Message)
            .collect())
    }

    /// 加载指定 thread 的元数据
    async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta>;

    /// 更新指定 thread 的元数据
    async fn update_meta(&self, id: &ThreadId, meta: ThreadMeta) -> Result<()>;

    /// 加载会话创建时冻结的版本化上下文快照。
    ///
    /// `None` 表示 legacy thread 尚未持久化快照；实现不得从 thread 列表投影
    /// 返回该大字段。默认用于旧测试替身，按 legacy 语义返回缺失。
    async fn load_frozen_snapshot(&self, _id: &ThreadId) -> Result<Option<String>> {
        Ok(None)
    }

    /// 仅当 thread 尚无快照时持久化会话创建时冻结的版本化上下文快照。
    ///
    /// 返回 true 表示本调用赢得 write-once；false 表示已有 winner，调用方
    /// 必须重读 canonical snapshot，不得覆盖。默认必须显式失败，禁止假成功。
    async fn store_frozen_snapshot_if_absent(
        &self,
        _id: &ThreadId,
        _snapshot: &str,
    ) -> Result<bool> {
        anyhow::bail!("unsupported frozen snapshot persistence")
    }

    /// 列举所有 thread 元数据，按 updated_at 降序（不含 hidden 的子 agent）
    async fn list_threads(&self) -> Result<Vec<ThreadMeta>>;

    /// 列举指定工作目录下可展示的非空 thread，不计算消息内容总大小。
    ///
    /// 存储实现应覆盖此方法并在数据源层完成过滤；默认实现用于测试替身和
    /// 非性能关键实现的兼容。
    async fn list_thread_entries(&self, cwd: &str) -> Result<Vec<ThreadListEntry>> {
        Ok(self
            .list_threads()
            .await?
            .into_iter()
            .filter(|meta| !meta.hidden && meta.message_count > 0 && meta.cwd == cwd)
            .map(ThreadListEntry::from)
            .collect())
    }

    /// 删除指定 thread（包含消息和元数据）
    async fn delete_thread(&self, id: &ThreadId) -> Result<()>;

    /// 更新指定 thread 的标题
    async fn update_title(&self, id: &ThreadId, title: &str) -> Result<()> {
        let mut meta = self.load_meta(id).await?;
        meta.title = Some(title.to_string());
        self.update_meta(id, meta).await
    }

    /// Load read-only inherited history separately from this thread's own payloads.
    /// Legacy stores may return no inherited context; snapshots must preserve the flags
    /// captured at child creation, never substitute the parent's current flags.
    async fn load_inherited_context(&self, _thread_id: &ThreadId) -> Result<InheritedContext> {
        Ok(InheritedContext::default())
    }

    /// Persist the child's inherited snapshot once. Unsupported stores must fail explicitly.
    async fn store_inherited_context(
        &self,
        _thread_id: &ThreadId,
        _context: &InheritedContext,
    ) -> Result<()> {
        anyhow::bail!("unsupported inherited context persistence")
    }

    /// 加载 thread 的完整逻辑上下文（含祖先链）。默认仅包装 legacy message context。
    async fn load_context_payloads(&self, thread_id: &ThreadId) -> Result<Vec<PersistedPayload>> {
        Ok(self
            .load_context(thread_id)
            .await?
            .into_iter()
            .map(PersistedPayload::Message)
            .collect())
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

#[cfg(test)]
mod persisted_payload_tests {
    use super::*;
    use crate::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SYSTEM_REMINDER_VERSION,
    };

    fn reminder_payload() -> PersistedPayload {
        let reminder = SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Security,
            source: ReminderSource("history_test".into()),
            kind: "notice".into(),
            severity: ReminderSeverity::Warning,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![ReminderAudience::Model, ReminderAudience::Tui]),
            body: "body".into(),
            summary: Some("summary".into()),
            metadata: serde_json::json!({"key": "value"}),
        };
        PersistedPayload::SystemReminder {
            id: MessageId::new(),
            reminder: TrustedSystemReminderFactory::for_producer()
                .construct(reminder)
                .unwrap(),
        }
    }

    #[test]
    fn reminder_envelope_roundtrip_preserves_id_and_fields() {
        let original = reminder_payload();
        let encoded = serialize_persisted_payload(&original).unwrap();
        let decoded = deserialize_persisted_payload(&encoded).unwrap();
        assert_eq!(decoded.id(), original.id());
        let PersistedPayload::SystemReminder { reminder, .. } = decoded else {
            panic!("expected reminder")
        };
        let dto = reminder.as_reminder();
        assert_eq!(dto.category, ReminderCategory::Security);
        assert_eq!(dto.summary.as_deref(), Some("summary"));
        assert_eq!(dto.metadata, serde_json::json!({"key": "value"}));
    }

    #[test]
    fn legacy_base_message_remains_readable() {
        let message = BaseMessage::human("legacy user content");
        let decoded =
            deserialize_persisted_payload(&serde_json::to_string(&message).unwrap()).unwrap();
        assert_eq!(
            decoded.as_message().unwrap().content(),
            "legacy user content"
        );
    }

    #[test]
    fn future_and_corrupt_reminders_fail_closed() {
        let encoded = serialize_persisted_payload(&reminder_payload()).unwrap();
        let mut future: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        future["version"] = serde_json::json!(99);
        assert!(deserialize_persisted_payload(&future.to_string()).is_err());
        future["version"] = serde_json::json!(PERSISTED_PAYLOAD_VERSION);
        future["reminder"]["body"] = serde_json::json!({"not": "text"});
        assert!(deserialize_persisted_payload(&future.to_string()).is_err());
    }
}
