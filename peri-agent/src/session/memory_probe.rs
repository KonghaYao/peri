//! 对象级内存计量原语与诊断采样。
//!
//! 本模块只回答「进程内仍然被哪些对象引用、按容量计大约保留了多少分配」，
//! 不是分配器统计，也不把正文长度冒充堆总量：
//!
//! - [`ObjectBytes::body`] 是已使用的正文/槽位字节（`String::len`、`Vec::len *
//!   size_of`），用于比较“可回收逻辑正文”的多少；
//! - [`ObjectBytes::reserved`] 是 `String::capacity` / `Vec::capacity` 给出的
//!   保留量下界，不含分配器元数据、`im` 共享节点、`HashMap` 桶开销与页粒度；
//! - 每个快照独立计量。跨快照比较只能说明“这些所有者不再引用这些正文”，
//!   不能单独证明分配器已经归还 RSS。
//!
//! 正文对象一旦离开快照的遍历范围，就说明该所有者不再持有它；若没有其他
//! 所有者（本模块不追踪的外部 clone），对应 `String`/`Vec` 分配随所有权消失
//! 而释放。测试据此断言容量下降与条目消失，RSS 仅作次要观察。

use std::collections::HashMap;

use peri_acp_types::messages::{
    BaseMessage, ContentBlock, DocumentSource, ImageSource, MessageContent,
};
use peri_acp_types::store::{MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use serde_json::Value;

use crate::messages::MessageId;

use super::transcript::{StagedData, TranscriptEntry};

// ─── 基础字节口径 ─────────────────────────────────────────────────────────────

/// 一组对象的内容字节与容量保留量（下界）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObjectBytes {
    /// 已使用正文与内联槽位字节（`String::len` / `Vec::len * size_of`）。
    pub body: usize,
    /// 按 `String::capacity` / `Vec::capacity` 计保留量下界。
    pub reserved: usize,
}

impl ObjectBytes {
    pub const ZERO: Self = Self {
        body: 0,
        reserved: 0,
    };
}

impl std::ops::Add for ObjectBytes {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            body: self.body + rhs.body,
            reserved: self.reserved + rhs.reserved,
        }
    }
}

impl std::ops::AddAssign for ObjectBytes {
    fn add_assign(&mut self, rhs: Self) {
        self.body += rhs.body;
        self.reserved += rhs.reserved;
    }
}

/// 单个 `String` 的正文与容量。
pub fn string_bytes(value: &String) -> ObjectBytes {
    ObjectBytes {
        body: value.len(),
        reserved: value.capacity(),
    }
}

/// 只持有 `&str` 时容量不可见；用长度作保留量下界，并在调用处注明。
fn borrowed_str_bytes(value: &str) -> ObjectBytes {
    ObjectBytes {
        body: value.len(),
        reserved: value.len(),
    }
}

fn vec_slots<T>(values: &Vec<T>) -> ObjectBytes {
    let element = std::mem::size_of::<T>();
    ObjectBytes {
        body: values.len() * element,
        reserved: values.capacity() * element,
    }
}

fn option_string_bytes(value: &Option<String>) -> ObjectBytes {
    value.as_ref().map(string_bytes).unwrap_or_default()
}

// ─── 消息 / reminder / payload ───────────────────────────────────────────────

fn json_bytes(value: &Value) -> ObjectBytes {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => ObjectBytes::ZERO,
        Value::String(text) => string_bytes(text),
        Value::Array(values) => {
            let mut bytes = vec_slots(values);
            for value in values {
                bytes += json_bytes(value);
            }
            bytes
        }
        Value::Object(map) => {
            // `serde_json::Map` 不保证暴露 capacity；用条目内联槽位作保留量下界。
            let mut bytes = ObjectBytes {
                body: 0,
                reserved: map.len() * std::mem::size_of::<(String, Value)>(),
            };
            for (key, value) in map {
                bytes += string_bytes(key);
                bytes += json_bytes(value);
            }
            bytes
        }
    }
}

fn image_source_bytes(source: &ImageSource) -> ObjectBytes {
    match source {
        ImageSource::Base64 { media_type, data } => string_bytes(media_type) + string_bytes(data),
        ImageSource::Url { url } => string_bytes(url),
    }
}

fn document_source_bytes(source: &DocumentSource) -> ObjectBytes {
    match source {
        DocumentSource::Base64 { media_type, data } => {
            string_bytes(media_type) + string_bytes(data)
        }
        DocumentSource::Url { url } => string_bytes(url),
        DocumentSource::Text { text } => string_bytes(text),
    }
}

fn content_block_bytes(block: &ContentBlock) -> ObjectBytes {
    match block {
        ContentBlock::Text { text } => string_bytes(text),
        ContentBlock::Image { source } => image_source_bytes(source),
        ContentBlock::Document { source, title } => {
            document_source_bytes(source) + option_string_bytes(title)
        }
        ContentBlock::ToolUse { id, name, input } => {
            string_bytes(id) + string_bytes(name) + json_bytes(input)
        }
        ContentBlock::ToolResult {
            id,
            tool_use_id,
            content,
            ..
        } => {
            let mut bytes =
                option_string_bytes(id) + string_bytes(tool_use_id) + vec_slots(content);
            for child in content {
                bytes += content_block_bytes(child);
            }
            bytes
        }
        ContentBlock::Reasoning { text, signature } => {
            string_bytes(text) + option_string_bytes(signature)
        }
        ContentBlock::Unknown(value) => json_bytes(value),
    }
}

fn message_content_bytes(content: &MessageContent) -> ObjectBytes {
    match content {
        MessageContent::Text(text) => string_bytes(text),
        MessageContent::Blocks(blocks) => {
            let mut bytes = vec_slots(blocks);
            for block in blocks {
                bytes += content_block_bytes(block);
            }
            bytes
        }
        MessageContent::Raw(values) => {
            let mut bytes = vec_slots(values);
            for value in values {
                bytes += json_bytes(value);
            }
            bytes
        }
    }
}

/// 单条 `BaseMessage` 的正文与容量保留量。
pub fn message_bytes(message: &BaseMessage) -> ObjectBytes {
    match message {
        BaseMessage::Human { content, .. } | BaseMessage::System { content, .. } => {
            message_content_bytes(content)
        }
        BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } => {
            let mut bytes = message_content_bytes(content) + vec_slots(tool_calls);
            for call in tool_calls {
                bytes += string_bytes(&call.id);
                bytes += string_bytes(&call.name);
                bytes += json_bytes(&call.arguments);
            }
            bytes
        }
        BaseMessage::Tool {
            tool_call_id,
            content,
            execution,
            subagent_failure,
            ..
        } => {
            let mut bytes = string_bytes(tool_call_id) + message_content_bytes(content);
            if let Some(execution) = execution {
                bytes += option_string_bytes(&execution.output_ref);
                bytes += option_string_bytes(&execution.task_id);
            }
            if let Some(failure) = subagent_failure {
                // 访问器只提供 `&str`；容量不可见，使用正文长度作保留量下界。
                bytes += borrowed_str_bytes(failure.child_thread_id());
                bytes += borrowed_str_bytes(failure.category_name());
                bytes += borrowed_str_bytes(failure.message());
                for cause in failure.causes() {
                    bytes += borrowed_str_bytes(cause);
                }
            }
            bytes
        }
    }
}

/// canonical reminder 的正文与容量保留量。
pub fn reminder_bytes(reminder: &TrustedSystemReminder) -> ObjectBytes {
    let reminder = reminder.as_reminder();
    string_bytes(&reminder.body)
        + string_bytes(&reminder.source.0)
        + string_bytes(&reminder.kind)
        + option_string_bytes(&reminder.summary)
        + json_bytes(&reminder.metadata)
        + vec_slots(&reminder.audiences.0)
}

/// 单条持久化载荷的正文与容量保留量。
pub fn payload_bytes(payload: &PersistedPayload) -> ObjectBytes {
    match payload {
        PersistedPayload::Message(message) => message_bytes(message),
        PersistedPayload::SystemReminder { reminder, .. } => reminder_bytes(reminder),
    }
}

pub(crate) fn transcript_entry_bytes(entry: &TranscriptEntry) -> ObjectBytes {
    match entry {
        TranscriptEntry::Message(message) => message_bytes(message),
        TranscriptEntry::Reminder { reminder, .. } => reminder_bytes(reminder),
    }
}

// ─── 快照 ────────────────────────────────────────────────────────────────────

/// `Vec<PersistedPayload>` 的有效条目与字节快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PayloadSetSnapshot {
    pub entries: usize,
    pub messages: usize,
    pub reminders: usize,
    pub bytes: ObjectBytes,
    /// `Vec` 容量槽位（不含分配器元数据与元素内部堆分配）。
    pub container_bytes: usize,
}

/// 采集持久化载荷集合快照。
///
/// `capacity` 由持有 `Vec` 的调用方传入；只持有切片时传 `slice.len()` 作为
/// 容量下界。
pub fn payload_set_snapshot(payloads: &[PersistedPayload], capacity: usize) -> PayloadSetSnapshot {
    let mut snapshot = PayloadSetSnapshot {
        entries: payloads.len(),
        container_bytes: capacity * std::mem::size_of::<PersistedPayload>(),
        ..Default::default()
    };
    for payload in payloads {
        snapshot.bytes += payload_bytes(payload);
        match payload {
            PersistedPayload::Message(_) => snapshot.messages += 1,
            PersistedPayload::SystemReminder { .. } => snapshot.reminders += 1,
        }
    }
    snapshot
}

/// `Vec<BaseMessage>` 的条目与字节快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MessageSetSnapshot {
    pub entries: usize,
    pub bytes: ObjectBytes,
    pub container_bytes: usize,
}

pub fn message_set_snapshot(messages: &[BaseMessage], capacity: usize) -> MessageSetSnapshot {
    let mut snapshot = MessageSetSnapshot {
        entries: messages.len(),
        container_bytes: capacity * std::mem::size_of::<BaseMessage>(),
        ..Default::default()
    };
    for message in messages {
        snapshot.bytes += message_bytes(message);
    }
    snapshot
}

/// Transcript 自身的对象计量：自有区与冻结继承区分开，容器容量单独报告。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TranscriptMemorySnapshot {
    pub own_entries: usize,
    pub inherited_entries: usize,
    pub total_entries: usize,
    pub active_entries: usize,
    pub excluded_entries: usize,
    pub own_bytes: ObjectBytes,
    pub inherited_bytes: ObjectBytes,
    /// Full 已标记但尚未收缩的条目正文（收缩前用于量化待释放量）。
    pub excluded_bytes: ObjectBytes,
    pub staged_bytes: ObjectBytes,
    pub entries_capacity: usize,
    pub entries_capacity_bytes: usize,
    pub id_index_capacity: usize,
    pub id_index_capacity_bytes: usize,
    pub flags_capacity: usize,
    pub flags_capacity_bytes: usize,
}

/// 采集 transcript 快照。
///
/// `ancestor_len` 之前的条目属于冻结继承区；容器容量由 transcript 自身传入，
/// 因为这里只拿到切片。
#[allow(clippy::too_many_arguments)]
pub fn transcript_snapshot(
    entries: &[TranscriptEntry],
    ancestor_len: usize,
    flags: &HashMap<MessageId, MessageFlags>,
    entries_capacity: usize,
    id_index_capacity: usize,
    flags_capacity: usize,
    staged: Option<&StagedData>,
) -> TranscriptMemorySnapshot {
    let mut snapshot = TranscriptMemorySnapshot {
        total_entries: entries.len(),
        entries_capacity,
        entries_capacity_bytes: entries_capacity * std::mem::size_of::<TranscriptEntry>(),
        id_index_capacity,
        id_index_capacity_bytes: id_index_capacity * std::mem::size_of::<(MessageId, usize)>(),
        flags_capacity,
        flags_capacity_bytes: flags_capacity * std::mem::size_of::<(MessageId, MessageFlags)>(),
        ..Default::default()
    };
    for (index, entry) in entries.iter().enumerate() {
        let bytes = transcript_entry_bytes(entry);
        if index < ancestor_len {
            snapshot.inherited_entries += 1;
            snapshot.inherited_bytes += bytes;
        } else {
            snapshot.own_entries += 1;
            snapshot.own_bytes += bytes;
        }
        let excluded = flags.get(&entry.id()).is_some_and(|flags| flags.excluded);
        if excluded {
            snapshot.excluded_entries += 1;
            snapshot.excluded_bytes += bytes;
        } else {
            snapshot.active_entries += 1;
        }
    }
    if let Some(staged) = staged {
        snapshot.staged_bytes = message_bytes(&staged.ai_message)
            + message_set_snapshot(&staged.tool_results, staged.tool_results.capacity()).bytes;
    }
    snapshot
}

// ─── 采样 ────────────────────────────────────────────────────────────────────

/// 统一 debug target；关闭 DEBUG 时调用方先经本函数短路，避免构造快照。
pub fn observations_enabled() -> bool {
    tracing::enabled!(target: "peri_memory", tracing::Level::DEBUG)
}

pub fn log_transcript(stage: &'static str, origin: &str, snapshot: &TranscriptMemorySnapshot) {
    tracing::debug!(
        target: "peri_memory",
        stage,
        origin,
        own_entries = snapshot.own_entries,
        inherited_entries = snapshot.inherited_entries,
        total_entries = snapshot.total_entries,
        active_entries = snapshot.active_entries,
        excluded_entries = snapshot.excluded_entries,
        own_body_bytes = snapshot.own_bytes.body,
        own_reserved_bytes = snapshot.own_bytes.reserved,
        inherited_body_bytes = snapshot.inherited_bytes.body,
        inherited_reserved_bytes = snapshot.inherited_bytes.reserved,
        excluded_body_bytes = snapshot.excluded_bytes.body,
        staged_body_bytes = snapshot.staged_bytes.body,
        entries_capacity_bytes = snapshot.entries_capacity_bytes,
        id_index_capacity_bytes = snapshot.id_index_capacity_bytes,
        flags_capacity_bytes = snapshot.flags_capacity_bytes,
        "object-memory transcript"
    );
}

pub fn log_transcript_shrink(
    stage: &'static str,
    origin: &str,
    before: &TranscriptMemorySnapshot,
    after: &TranscriptMemorySnapshot,
) {
    tracing::debug!(
        target: "peri_memory",
        stage,
        origin,
        own_entries_before = before.own_entries,
        own_entries_after = after.own_entries,
        own_body_before = before.own_bytes.body,
        own_body_after = after.own_bytes.body,
        own_reserved_before = before.own_bytes.reserved,
        own_reserved_after = after.own_bytes.reserved,
        inherited_body_before = before.inherited_bytes.body,
        inherited_body_after = after.inherited_bytes.body,
        entries_capacity_before = before.entries_capacity_bytes,
        entries_capacity_after = after.entries_capacity_bytes,
        id_index_capacity_before = before.id_index_capacity_bytes,
        id_index_capacity_after = after.id_index_capacity_bytes,
        flags_capacity_before = before.flags_capacity_bytes,
        flags_capacity_after = after.flags_capacity_bytes,
        "object-memory full commit shrink"
    );
}

pub fn log_payload_set(stage: &'static str, origin: &str, snapshot: &PayloadSetSnapshot) {
    tracing::debug!(
        target: "peri_memory",
        stage,
        origin,
        entries = snapshot.entries,
        messages = snapshot.messages,
        reminders = snapshot.reminders,
        body_bytes = snapshot.bytes.body,
        reserved_bytes = snapshot.bytes.reserved,
        container_bytes = snapshot.container_bytes,
        "object-memory payload set"
    );
}

pub fn log_payload_adoption(origin: &str, before: &PayloadSetSnapshot, after: &PayloadSetSnapshot) {
    tracing::debug!(
        target: "peri_memory",
        stage = "host_adopt",
        origin,
        entries_before = before.entries,
        entries_after = after.entries,
        body_before = before.bytes.body,
        body_after = after.bytes.body,
        reserved_before = before.bytes.reserved,
        reserved_after = after.bytes.reserved,
        container_before = before.container_bytes,
        container_after = after.container_bytes,
        "object-memory host adoption"
    );
}

pub fn log_message_set(stage: &'static str, origin: &str, snapshot: &MessageSetSnapshot) {
    tracing::debug!(
        target: "peri_memory",
        stage,
        origin,
        entries = snapshot.entries,
        body_bytes = snapshot.bytes.body,
        reserved_bytes = snapshot.bytes.reserved,
        container_bytes = snapshot.container_bytes,
        "object-memory message set"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_snapshot_separates_body_from_capacity() {
        let mut text = String::with_capacity(128);
        text.push_str("abc");
        let payload = PersistedPayload::Message(BaseMessage::human(text));
        let snapshot = payload_set_snapshot(std::slice::from_ref(&payload), 4);
        assert_eq!(snapshot.bytes.body, 3);
        assert_eq!(snapshot.bytes.reserved, 128);
        assert_eq!(
            snapshot.container_bytes,
            4 * std::mem::size_of::<PersistedPayload>()
        );
        assert_eq!(snapshot.entries, 1);
        assert_eq!(snapshot.messages, 1);
    }

    #[test]
    fn transcript_snapshot_splits_inherited_own_and_excluded() {
        let inherited = TranscriptEntry::Message(BaseMessage::human("parent"));
        let own = BaseMessage::human("own");
        let own_id = own.id();
        let entries = vec![inherited, TranscriptEntry::Message(own)];
        let flags = HashMap::from([(
            own_id,
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        )]);
        let snapshot = transcript_snapshot(&entries, 1, &flags, entries.capacity(), 2, 1, None);
        assert_eq!(snapshot.inherited_entries, 1);
        assert_eq!(snapshot.own_entries, 1);
        assert_eq!(snapshot.active_entries, 1);
        assert_eq!(snapshot.excluded_entries, 1);
        assert!(snapshot.inherited_bytes.body >= "parent".len());
        assert!(snapshot.excluded_bytes.body >= "own".len());
    }
}
