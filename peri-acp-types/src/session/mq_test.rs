//! Thread MQ 契约的纯规则：`(enqueued_at, id)` 水位与行到水位的映射。

use super::{MessageKind, MessagePolicy, MessageSource, MqEntry, MqWatermark};
use crate::messages::{BaseMessage, MessageId};
use crate::store::PersistedPayload;

fn entry(enqueued_at: &str, id: MessageId) -> MqEntry {
    MqEntry {
        id,
        enqueued_at: enqueued_at.to_owned(),
        kind: MessageKind::Prompt,
        source: MessageSource::UserInput,
        payload: PersistedPayload::Message(BaseMessage::Human {
            id,
            content: "text".into(),
        }),
        policy: MessagePolicy::ensure_processing(),
        original_draft: None,
    }
}

/// 时间优先，时间相同按消息 ID 稳定排序：不引入序号，也不改写时间。
#[test]
fn watermark_orders_by_time_then_message_id() {
    let earlier = MessageId::new();
    let later = MessageId::new();
    let first = MqWatermark::new("2026-11-01T10:00:00.000000Z", earlier);
    let second = MqWatermark::new("2026-11-01T10:00:01.000000Z", later);
    assert!(first < second);
    assert!(second > first);

    let same_time_a = MqWatermark::new("2026-11-01T10:00:02.000000Z", earlier);
    let same_time_b = MqWatermark::new("2026-11-01T10:00:02.000000Z", later);
    assert_eq!(
        same_time_a.cmp(&same_time_b),
        earlier.as_uuid().cmp(&later.as_uuid()),
        "同一时间戳只能靠消息 ID 定序"
    );
    assert_ne!(same_time_a, same_time_b);
}

/// 行的水位就是它自己那对事实，拼接不引入第二个身份。
#[test]
fn entry_watermark_is_the_row_key() {
    let id = MessageId::new();
    let row = entry("2026-11-01T10:00:00.500000Z", id);
    let watermark = row.watermark();
    assert_eq!(watermark.enqueued_at, row.enqueued_at);
    assert_eq!(watermark.id, row.id);
    assert_eq!(row.payload.id(), id, "载荷与 MQ 行共用同一个稳定 ID");
}
