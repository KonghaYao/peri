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
use crate::system_reminder;
use crate::system_reminder::SystemReminder;
use crate::system_reminder::TrustedSystemReminder;
use crate::system_reminder::TrustedSystemReminderFactory;
use crate::thread::{ThreadId, ThreadListEntry, ThreadMeta, WorkState};

pub use thread_store::PERSISTED_PAYLOAD_VERSION;

pub use thread_store::PersistedPayload;

pub use thread_store::serialize_persisted_payload;

pub use thread_store::serialize_persisted_payload_with_selections;

pub use thread_store::deserialize_persisted_payload;

pub use thread_store::deserialize_persisted_payload_with_selections;

pub use thread_store::InheritedContext;

pub use thread_store::InheritedPayloadSpan;

pub use thread_store::InheritedSerialization;

pub use thread_store::CompactionChange;

pub use thread_store::MessageFlags;

pub use thread_store::ThreadStore;

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

    /// MQ 待发行用 payload JSON 的附带字段持久化插件选择；历史读取不暴露该字段。
    #[test]
    fn plugin_selections_round_trip_inside_payload_envelope() {
        use crate::session::PluginSelection;

        let payload = PersistedPayload::Message(BaseMessage::human("select alpha"));
        let selections = vec![
            PluginSelection::new("alpha"),
            PluginSelection::new("beta@market"),
        ];
        let encoded = serialize_persisted_payload_with_selections(&payload, &selections).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            value["pluginSelections"],
            serde_json::json!([{"pluginId": "alpha"}, {"pluginId": "beta@market"}])
        );

        let (restored, restored_selections) =
            deserialize_persisted_payload_with_selections(&encoded).unwrap();
        assert_eq!(restored.id(), payload.id());
        assert_eq!(restored_selections, selections);
        assert_eq!(
            deserialize_persisted_payload(&encoded).unwrap().id(),
            payload.id(),
            "历史读取仍按原语义只取 payload"
        );

        let legacy = serialize_persisted_payload(&payload).unwrap();
        let (_, empty) = deserialize_persisted_payload_with_selections(&legacy).unwrap();
        assert!(empty.is_empty(), "旧行缺省为空选择");
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
        let paired_payload = serialization.payloads.iter().zip(&context.payloads);
        for (span, payload) in paired_payload {
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

#[path = "thread_store.rs"]
mod thread_store;
