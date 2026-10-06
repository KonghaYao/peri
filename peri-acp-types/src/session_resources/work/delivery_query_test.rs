use super::WorkDeliveryQuery;

#[test]
fn delivery_query_uses_explicit_camel_case_identity() {
    let query = WorkDeliveryQuery {
        session_id: "session".into(),
        delivery_id: "delivery.\"雪".into(),
    };
    let serialized = serde_json::to_value(&query).unwrap();
    assert_eq!(
        serialized,
        serde_json::json!({"sessionId": "session", "deliveryId": "delivery.\"雪"})
    );
    assert_eq!(
        serde_json::from_value::<WorkDeliveryQuery>(serialized).unwrap(),
        query
    );
}

#[test]
fn delivery_query_rejects_missing_or_unknown_identity_fields() {
    for serialized in [
        serde_json::json!({"sessionId": "session"}),
        serde_json::json!({"deliveryId": "delivery"}),
        serde_json::json!({"sessionId": "session", "deliveryId": "delivery", "limit": 1}),
    ] {
        assert!(serde_json::from_value::<WorkDeliveryQuery>(serialized).is_err());
    }
}
