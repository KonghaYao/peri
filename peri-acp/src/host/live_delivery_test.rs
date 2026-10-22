//! History V2 实时投递注册表：暂停缓冲、序号连续性、溢出缺口与关闭语义。

use super::*;
use serde_json::json;

fn text_payload(text: &str) -> Value {
    json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": text },
        },
    })
}

fn delivery(payload: &Value) -> &Value {
    &payload["_meta"][DELIVERY_META_KEY]
}

#[test]
fn paused_updates_are_buffered_until_resume() {
    let live = LiveDelivery::new();
    let boundary = live.begin("s1");
    assert_eq!(boundary.after_seq.get(), 0);
    assert!(live.is_paused("s1"));

    assert!(matches!(
        live.publish("s1", text_payload("one")),
        PublishOutcome::Buffered
    ));
    assert_eq!(live.buffered_len("s1"), 1);

    let pending = live.resume_and_take("s1");
    assert_eq!(pending.len(), 1);
    assert!(!live.is_paused("s1"));
    assert_eq!(
        delivery(&pending[0])["eventSeq"],
        "1",
        "恢复后的第一条从 afterSeq + 1 开始"
    );
    assert_eq!(
        delivery(&pending[0])["streamId"],
        boundary.stream_id.as_str()
    );
    assert_eq!(
        delivery(&pending[0])["deliveryId"],
        format!("{}:1", boundary.stream_id).as_str()
    );
    assert!(live.resume_and_take("s1").is_empty(), "缓冲只取一次");
}

#[test]
fn live_sequence_is_monotonic_and_delivery_ids_are_unique() {
    let live = LiveDelivery::new();
    let boundary = live.begin("s1");
    assert!(live.resume_and_take("s1").is_empty());

    let mut seqs = Vec::new();
    let mut ids = Vec::new();
    for index in 0..3 {
        let PublishOutcome::Send(payload) = live.publish("s1", text_payload(&index.to_string()))
        else {
            panic!("活动流必须直发");
        };
        let meta = delivery(&payload);
        seqs.push(meta["eventSeq"].as_str().unwrap().to_owned());
        ids.push(meta["deliveryId"].as_str().unwrap().to_owned());
        assert_eq!(
            meta["streamId"].as_str().unwrap(),
            boundary.stream_id.as_str()
        );
    }
    assert_eq!(seqs, ["1", "2", "3"]);
    assert_eq!(ids.len(), 3);
    assert!(ids.iter().all(|id| id.starts_with(&boundary.stream_id)));
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "deliveryId 不得重复：{ids:?}");
}

#[test]
fn same_session_reload_replaces_stream_and_keeps_sequence_boundary() {
    let live = LiveDelivery::new();
    let first = live.begin("s1");
    live.resume_and_take("s1");
    for _ in 0..2 {
        let PublishOutcome::Send(_) = live.publish("s1", text_payload("x")) else {
            panic!("活动流必须直发");
        };
    }

    let second = live.begin("s1");
    assert_ne!(first.stream_id, second.stream_id, "重载必须换流身份");
    assert_eq!(second.after_seq.get(), 2, "新边界沿用已发出的最后序号");
    assert!(!live.has_stream_with_id("s1", &first.stream_id));
    assert!(live.has_stream_with_id("s1", &second.stream_id));

    let pending = live.resume_and_take("s1");
    assert!(pending.is_empty());
    let PublishOutcome::Send(payload) = live.publish("s1", text_payload("after")) else {
        panic!("活动流必须直发");
    };
    assert_eq!(delivery(&payload)["eventSeq"], "3");
}

#[test]
fn close_removes_stream_and_later_updates_pass_through() {
    let live = LiveDelivery::new();
    live.begin("s1");
    live.close("s1");
    assert!(!live.has_stream("s1"));

    let PublishOutcome::Send(payload) = live.publish("s1", text_payload("plain")) else {
        panic!("关闭后的流不得再缓冲");
    };
    assert!(
        payload.get("_meta").is_none(),
        "流不存在时不得附加投递元数据：{payload}"
    );
    assert!(live.resume_and_take("s1").is_empty());
}

#[test]
fn close_if_paused_never_tears_down_an_active_stream() {
    let live = LiveDelivery::new();
    live.begin("s1");
    live.close_if_paused("s1");
    assert!(!live.has_stream("s1"), "暂停流可被清理");

    live.begin("s1");
    live.resume_and_take("s1");
    live.close_if_paused("s1");
    assert!(live.has_stream("s1"), "直发流不得因失败的 load 被拆除");
}

#[test]
fn count_overflow_drops_oldest_and_exposes_gap() {
    let live = LiveDelivery::new();
    let boundary = live.begin("s1");
    let mut overflowed = false;
    for index in 0..(MAX_BUFFERED_UPDATES + 10) {
        let outcome = live.publish("s1", text_payload(&index.to_string()));
        overflowed |= matches!(outcome, PublishOutcome::Overflow);
    }
    assert!(overflowed, "超过条数上限必须报告 Overflow");
    assert_eq!(live.buffered_len("s1"), MAX_BUFFERED_UPDATES);

    let pending = live.resume_and_take("s1");
    assert_eq!(pending.len(), MAX_BUFFERED_UPDATES);
    let first_seq: u64 = delivery(&pending[0])["eventSeq"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        first_seq,
        (MAX_BUFFERED_UPDATES + 10 - MAX_BUFFERED_UPDATES) as u64 + 1,
        "丢弃 10 条后第一条序号必须是 afterSeq + 11，客户端据缺口进入 detached"
    );
    assert_eq!(boundary.after_seq.get(), 0);
    let last_seq: u64 = delivery(&pending[pending.len() - 1])["eventSeq"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(last_seq, (MAX_BUFFERED_UPDATES + 10) as u64);
}

#[test]
fn byte_overflow_keeps_buffer_bounded() {
    let live = LiveDelivery::new();
    live.begin("s1");
    let big = "x".repeat(600 * 1024);
    assert!(matches!(
        live.publish("s1", text_payload(&big)),
        PublishOutcome::Buffered
    ));
    assert!(matches!(
        live.publish("s1", text_payload(&big)),
        PublishOutcome::Overflow
    ));
    assert_eq!(live.buffered_len("s1"), 1, "超字节上限时丢最旧");
    assert!(
        live.buffered_bytes("s1") <= MAX_BUFFERED_BYTES,
        "缓冲字节不得无界增长"
    );

    let pending = live.resume_and_take("s1");
    assert_eq!(pending.len(), 1);
    assert_eq!(
        delivery(&pending[0])["eventSeq"],
        "2",
        "被丢弃的第一条留下序号缺口"
    );
}

#[test]
fn absent_stream_sends_payload_unchanged() {
    let live = LiveDelivery::new();
    let payload = text_payload("plain");
    let PublishOutcome::Send(sent) = live.publish("unknown", payload.clone()) else {
        panic!("未知会话必须透传");
    };
    assert_eq!(sent, payload);
}

#[test]
fn close_all_discards_every_stream() {
    let live = LiveDelivery::new();
    live.begin("s1");
    live.begin("s2");
    live.close_all();
    assert!(!live.has_stream("s1"));
    assert!(!live.has_stream("s2"));
}
