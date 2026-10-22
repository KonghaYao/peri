//! ThreadStore 契约 — 持久化存储抽象（自 peri-agent/src/thread 下沉）。
//!
//! 接口契约归 peri-acp-types：`SqliteThreadStore`（peri-resources）实现本 trait，
//! Agent/ACP/TUI 经本 trait 引用存储，不直接实例化。
//!
//! 本 trait 是**迁移桥**：目标契约是 [`crate::session_resources::SessionResources`]
//! （会话行为门面，数据与本机执行两面分离）。在消费侧迁移完成前保留本 trait 不动
//! 摇现有调用方；迁移期间遵守两条约束：
//!
//! 1. 不为新场景扩展本 trait；新行为加到 `SessionResources`。
//! 2. 不新增返回 no-op / 空值 / `None` 的默认实现冒充「不支持」——无法完成的行为
//!    必须显式失败，否则调用方无法区分「不支持」与「确实不存在」。
//!
//! 纯历史规则（fork 重映射、投影 flag、compaction 批次、rewind 边界）见
//! [`history`]，不得在调用方或 adapter 内各写一份。

pub mod history;

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::messages::{BaseMessage, MessageId};
use crate::projection::MessageProjectionDirective;
use crate::system_reminder::{
    decode_system_reminder_json, reminder_egress_allowed, SystemReminder, TrustedSystemReminder,
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

    /// 客户端可展示性：System 条目与 Model-only reminder 没有客户端渲染形式。
    ///
    /// 与回放出口的过滤同源（`dispatch::session_replay` 的 `!is_system()` 与
    /// `TransportEventSink::push_system_reminder` 的受众判定）。历史分页据此跳过这类行，
    /// 但**不影响它的读取位置**：位置按行推进，展示按本条规则过滤。
    pub fn displayable_to_client(&self) -> bool {
        match self {
            Self::Message(message) => !message.is_system(),
            Self::SystemReminder { reminder, .. } => reminder_egress_allowed(
                reminder.as_reminder(),
                crate::system_reminder::ReminderAudience::Tui,
            ),
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
        message: Box<BaseMessage>,
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
        RawPersistedPayload::Message { message } => Ok(PersistedPayload::Message(*message)),
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

/// 一条 payload 在 inherited 列序列化文本中的定位事实。
///
/// `raw_start` / `raw_len` 指向信封里转义后的字符串内容（不含两端引号），与
/// `peri-resources` 的 blob 扫描同一口径；`logical_len` 是解码后的 UTF-8 字节数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InheritedPayloadSpan {
    pub id: MessageId,
    pub raw_start: u64,
    pub raw_len: u64,
    pub logical_len: u64,
}

/// [`InheritedContext::to_json_with_spans`] 的结果：序列化文本与逐 payload 定位。
#[derive(Clone, Debug)]
pub struct InheritedSerialization {
    pub json: String,
    pub payloads: Vec<InheritedPayloadSpan>,
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

    /// 序列化同时给出每条 payload 在列内的定位事实（写时派生索引的输入）。
    ///
    /// 信封布局是 `serde_json` 的紧凑输出：`{"version":1,"payloads":["...",...],"flags":{...}}`。
    /// 这里逐条重新转义并与实际输出逐字节核对；布局与 canonical 形状不一致时返回错误，
    /// 而不是产出不可验证的区间。
    pub fn to_json_with_spans(&self) -> Result<InheritedSerialization> {
        let payloads: Vec<String> = self
            .payloads
            .iter()
            .map(serialize_persisted_payload)
            .collect::<Result<_>>()?;
        let json = serde_json::to_string(&InheritedContextEnvelope {
            version: 1,
            payloads: payloads.clone(),
            flags: self.flags.clone(),
        })?;
        const PREFIX: &[u8] = br#"{"version":1,"payloads":["#;
        let bytes = json.as_bytes();
        if !bytes.starts_with(PREFIX) {
            anyhow::bail!("inherited context serialization is not canonical");
        }
        let mut cursor = PREFIX.len();
        let mut spans = Vec::with_capacity(payloads.len());
        for (index, payload) in payloads.iter().enumerate() {
            let escaped = serde_json::to_string(payload)?;
            let Some(content) = escaped.as_bytes().get(1..escaped.len().saturating_sub(1)) else {
                anyhow::bail!("inherited payload serialization is not a JSON string");
            };
            if bytes.get(cursor) != Some(&b'"')
                || bytes.get(cursor + 1..cursor + 1 + content.len()) != Some(content)
                || bytes.get(cursor + 1 + content.len()) != Some(&b'"')
            {
                anyhow::bail!("inherited context serialization does not match its payloads");
            }
            spans.push(InheritedPayloadSpan {
                id: self.payloads[index].id(),
                raw_start: (cursor + 1) as u64,
                raw_len: content.len() as u64,
                logical_len: payload.len() as u64,
            });
            cursor += escaped.len();
            if index + 1 < payloads.len() {
                if bytes.get(cursor) != Some(&b',') {
                    anyhow::bail!("inherited context payload separator drifted");
                }
                cursor += 1;
            }
        }
        if bytes.get(cursor) != Some(&b']') {
            anyhow::bail!("inherited context payload array is not terminated");
        }
        Ok(InheritedSerialization {
            json,
            payloads: spans,
        })
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

/// 一次 compaction 的领域变更：flags 更新 + 摘要追加。
///
/// 名字描述领域事实而不是提交机制：调用方给出「这次 compact 改变了什么」，
/// 由 adapter 保证整体生效或整体不生效（原名 `CompactionLifecycle` 隐含了
/// 「调用方管理提交」的机制含义，已废弃）。
///
/// 纯应用规则见 [`history::apply_compaction_change`]。
#[derive(Clone, Debug)]
pub struct CompactionChange {
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
    ///
    /// 它同时是 flags 的持久化载荷：Full Compact 归档时写入的取代关系
    /// （[`MessageProjectionDirective::superseded_by`]）也经这里往来，
    /// 读取请用 [`MessageFlags::superseded_by`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<MessageProjectionDirective>,
}

impl MessageFlags {
    /// Whether this row is recovery storage rather than executable history.
    pub fn is_recovery_only(&self) -> bool {
        self.projection
            .as_ref()
            .is_some_and(|directive| directive.recovery_only)
    }

    /// 取代本条的摘要消息 id（Full Compact 归档时写入）。
    ///
    /// 只有 `excluded` 为真时才有意义：它是「本条被哪条摘要取代」这条事实的读取口，
    /// 供 rewind 精确判定排除是否仍有依据（[`history::rewind_clear_excluded_ids`]）。
    /// 未记录（本字段落地前归档的历史）返回 `None`，调用方在该情形下沿用保守判定。
    pub fn superseded_by(&self) -> Option<MessageId> {
        self.projection
            .as_ref()
            .and_then(|directive| directive.superseded_by)
    }
}

#[async_trait]
pub trait ThreadStore: Send + Sync {
    /// Resolve and register one local execution workspace.
    async fn resolve_workspace(
        &self,
        _cwd: &std::path::Path,
    ) -> Result<crate::workspace::ResolvedWorkspace> {
        Err(crate::workspace::WorkspaceError::Unsupported.into())
    }

    /// Atomically create a thread with an immutable, validated execution binding.
    async fn create_bound_thread(
        &self,
        _meta: ThreadMeta,
        _workspace: &crate::workspace::ResolvedWorkspace,
    ) -> Result<ThreadId> {
        Err(crate::workspace::WorkspaceError::Unsupported.into())
    }

    async fn load_session_binding(
        &self,
        _id: &ThreadId,
    ) -> Result<Option<crate::workspace::SessionBinding>> {
        Ok(None)
    }

    /// Adopt an unbound root using its unchanged saved cwd and a validated workspace.
    /// Binding and the missing frozen snapshot commit atomically; existing values never change.
    async fn adopt_legacy_thread(
        &self,
        _id: &ThreadId,
        _saved_cwd: &str,
        _workspace: &crate::workspace::ResolvedWorkspace,
        _frozen_snapshot: &str,
    ) -> Result<()> {
        Err(crate::workspace::WorkspaceError::Unsupported.into())
    }

    /// 已有绑定的权威复核：归属关系与执行目录可用性。
    ///
    /// 一次准入只应调用一次（准入以它为判定的全部依据）；准入内的后续检查用
    /// [`ThreadStore::reassert_session_binding`]。文件对象身份与 Git 布局不参与准入。
    async fn validate_session_binding(
        &self,
        _id: &ThreadId,
    ) -> Result<crate::workspace::ResolvedWorkspace> {
        Err(crate::workspace::WorkspaceError::Unsupported.into())
    }

    /// 同一次准入内的复核：与权威复核同一套判定，不启动外部进程。
    ///
    /// 绑定不存在、workspace 关系不一致或执行目录不可用时仍然失败；目录被替换、
    /// Git 布局变化不再影响准入。
    async fn reassert_session_binding(
        &self,
        _id: &ThreadId,
    ) -> Result<crate::workspace::ResolvedWorkspace> {
        Err(crate::workspace::WorkspaceError::Unsupported.into())
    }

    async fn list_scoped_threads(
        &self,
        _query: &crate::workspace::ScopedThreadQuery,
    ) -> Result<crate::workspace::ScopedThreadPage> {
        Err(crate::workspace::WorkspaceError::Unsupported.into())
    }

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

    /// 加载 thread 的完整上下文（含祖先链）
    async fn load_context(&self, thread_id: &ThreadId) -> Result<Vec<BaseMessage>>;

    /// 列举指定父 thread 的直接子 thread
    async fn list_child_threads(&self, parent_id: &ThreadId) -> Result<Vec<ThreadMeta>>;

    /// 递归列举以 root_id 为根的所有 thread（含自身）
    async fn list_session_threads(&self, root_id: &ThreadId) -> Result<Vec<ThreadMeta>>;

    /// 更新 thread 的 agent_status 字段
    async fn update_thread_status(&self, id: &ThreadId, status: &str) -> Result<()>;

    /// 按 message_id 列表精确删除消息，并刷新派生计数。
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
        lifecycle: &CompactionChange,
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

#[cfg(test)]
mod inherited_serialization_tests {
    use super::*;

    fn escaped_payloads() -> Vec<PersistedPayload> {
        [
            "plain",
            "quotes \" and backslash \\ inside",
            "line\nbreak\tand \u{1} control",
            "中文与 emoji 🚀",
            "",
        ]
        .into_iter()
        .map(|content| PersistedPayload::Message(BaseMessage::human(content)))
        .collect()
    }

    /// 区间指向列文本里**转义后**的 payload 内容；逐条解码必须还原原始序列化。
    #[test]
    fn spans_point_at_escaped_bytes_and_round_trip() {
        let payloads = escaped_payloads();
        let context = InheritedContext {
            flags: std::collections::HashMap::from([(payloads[0].id(), MessageFlags::default())]),
            payloads,
        };
        let serialization = context.to_json_with_spans().unwrap();
        assert_eq!(serialization.payloads.len(), context.payloads.len());
        assert_eq!(serialization.json, context.to_json().unwrap());
        for (span, payload) in serialization.payloads.iter().zip(&context.payloads) {
            assert_eq!(span.id, payload.id());
            let serialized = serialize_persisted_payload(payload).unwrap();
            let start = usize::try_from(span.raw_start).unwrap();
            let len = usize::try_from(span.raw_len).unwrap();
            let end = start.checked_add(len).unwrap();
            assert!(end <= serialization.json.len());
            // 两端引号之间的内容就是重新转义后的 payload。
            let quoted = &serialization.json.as_bytes()[start - 1..end + 1];
            let decoded: String = serde_json::from_slice(quoted).unwrap();
            assert_eq!(decoded, serialized);
            assert_eq!(span.logical_len, serialized.len() as u64);
        }
        let decoded = InheritedContext::from_json(&serialization.json).unwrap();
        assert_eq!(
            decoded
                .payloads
                .iter()
                .map(PersistedPayload::id)
                .collect::<Vec<_>>(),
            context
                .payloads
                .iter()
                .map(PersistedPayload::id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn empty_payloads_have_no_spans() {
        let serialization = InheritedContext::default().to_json_with_spans().unwrap();
        assert!(serialization.payloads.is_empty());
        assert!(serialization.json.contains(r#""payloads":[]"#));
    }
}
