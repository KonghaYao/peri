use serde_json::json;

use super::*;

fn item(seq: u64) -> ItemView {
    ItemView {
        id: "itim_test".to_owned(),
        seq,
        item_id: "itim_test".to_owned(),
        kind: ItemKind::Message,
        source: ItemSource::root(),
        payload: json!({ "text": "hello" }),
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        truncated: false,
    }
}

#[test]
fn payload_within_limit_is_unchanged() {
    let payload = json!({ "text": "hello" });
    let (fitted, truncated) = ItemView::fit_payload(payload.clone());
    assert_eq!(fitted, payload);
    assert!(!truncated);
}

#[test]
fn oversized_payload_keeps_identity_and_original_size() {
    let payload = json!({ "text": "x".repeat(MAX_PAYLOAD_BYTES + 1) });
    let (fitted, truncated) = ItemView::fit_payload(payload);
    assert!(truncated);
    assert_eq!(fitted["_truncated"], json!(true));
    let original = fitted["originalBytes"].as_u64().expect("bytes");
    assert!(original > MAX_PAYLOAD_BYTES as u64);
}

#[test]
fn item_serializes_with_camel_case_fields() {
    let value = serde_json::to_value(item(7)).expect("item");
    assert_eq!(value["id"], json!("itim_test"));
    assert_eq!(value["seq"], json!(7));
    assert_eq!(value["itemId"], json!("itim_test"));
    assert_eq!(value["kind"], json!("message"));
    assert_eq!(value["source"]["agentId"], json!("root"));
    assert_eq!(value["source"]["taskId"], json!(null));
    assert_eq!(value["createdAt"], json!("2026-01-01T00:00:00.000000Z"));
    assert_eq!(value["truncated"], json!(false));
}

#[test]
fn item_page_exposes_cursor_and_has_more() {
    let page = ItemPage::new(
        "agent-01",
        vec![item(1), item(2)],
        Some(2),
        9,
        true,
        ItemOrder::Asc,
    );
    let value = serde_json::to_value(page.clone()).expect("page");
    assert_eq!(value["object"], json!("list"));
    assert_eq!(value["logicalId"], json!("agent-01"));
    assert_eq!(value["nextCursor"], json!(2));
    assert_eq!(value["lastItemSeq"], json!(9));
    assert_eq!(value["hasMore"], json!(true));
    assert_eq!(value["has_more"], json!(true));
    assert_eq!(value["order"], json!("asc"));
    // The official `CursorPage` reader consumes `data`; existing clients keep
    // reading `items`. Both are the same array.
    assert_eq!(value["data"], value["items"]);
    assert_eq!(value["data"].as_array().map(Vec::len), Some(2));

    let decoded: ItemPage = serde_json::from_value(value).expect("round trip");
    assert_eq!(
        decoded, page,
        "the duplicate spelling is not a second field"
    );
}

#[test]
fn order_parser_rejects_unknown_values() {
    assert_eq!(ItemOrder::parse("asc").expect("asc"), ItemOrder::Asc);
    assert_eq!(ItemOrder::parse("desc").expect("desc"), ItemOrder::Desc);
    assert_eq!(
        ItemOrder::parse("sideways")
            .expect_err("invalid order")
            .code(),
        crate::domain::ErrorCode::InvalidQuery
    );
}
