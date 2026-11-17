//! History V2 契约测试的补充组：正文块形状与 opaque 占位子项。
//!
//! 从 `history_v2_test.rs` 拆出以维持单文件 ≤1000 行；`use super::*` 复用父模块夹具。

use super::*;

fn body_chunk(complete: bool, offset: u64, next: Option<u64>) -> HistoryBodyChunk {
    HistoryBodyChunk {
        version: 2,
        kind: HistoryBodyKind::Body,
        session_id: "s1".to_owned(),
        window_id: "w1".to_owned(),
        snapshot_token: "snapshot-1".to_owned(),
        body_ref: "body-1".to_owned(),
        content_version: "v-content-1".to_owned(),
        representation: HistoryRepresentation::DisplayText,
        encoding: HistoryEncoding::Base64,
        offset: DecimalU64::new(offset),
        data: String::new(),
        next_offset: next.map(DecimalU64::new),
        complete,
        total_bytes: DecimalU64::new(10),
    }
}

#[test]
fn body_chunk_shape_rules() {
    let complete_with_next = body_chunk(true, 0, Some(10));
    assert!(complete_with_next.validate_shape().is_err());

    let incomplete_without_next = body_chunk(false, 0, None);
    assert!(incomplete_without_next.validate_shape().is_err());

    let not_advancing = body_chunk(false, 10, Some(10));
    assert!(not_advancing.validate_shape().is_err());

    let progressing = body_chunk(false, 0, Some(10));
    progressing.validate_shape().unwrap();

    let eof = body_chunk(true, 0, None);
    eof.validate_shape().unwrap();
}

#[test]
fn body_chunk_serializes_authoritative_total() {
    let chunk = body_chunk(true, 0, None);
    let value = serde_json::to_value(&chunk).unwrap();
    assert_eq!(value["totalBytes"], "10");
}

#[test]
fn opaque_subitem_carries_only_a_bounded_descriptor() {
    let opaque = all_sub_items().pop().expect("opaque last");
    let value = serde_json::to_value(&opaque).unwrap();
    assert_eq!(value["kind"], "opaque");
    assert_eq!(value["subItemId"], "opaque-0");
    assert_eq!(value["body"]["bodyRef"], "message-43");
    assert_eq!(value["body"]["representation"], "display-json");
    assert_eq!(value["body"]["mediaType"], "application/json");
    assert_eq!(value["body"]["totalBytes"], "104857600");
    assert_eq!(value["body"]["chunkBytes"], 65536);
    assert!(
        value.get("text").is_none() && value.get("output").is_none(),
        "opaque must not inline body bytes: {value}"
    );
    opaque.validate_bounded(&limits()).unwrap();

    let mut invalid = value;
    invalid["body"]["chunkBytes"] = json!(0);
    let item: HistorySubItem = serde_json::from_value(invalid).unwrap();
    assert!(item.validate_bounded(&limits()).is_err());
}
