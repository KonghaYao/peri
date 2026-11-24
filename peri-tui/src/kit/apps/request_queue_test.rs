use super::*;
use serde_json::json;

fn request(seq: u64) -> PendingAppRequest {
    PendingAppRequest {
        seq,
        id: json!(seq),
        method: "tools/call".to_string(),
        params: json!({"name": "get-time"}),
    }
}

#[test]
fn wire_decode_keeps_forwardable_fields() {
    let decoded = PendingAppRequest::from_wire(json!({
        "seq": 3,
        "id": "rpc-1",
        "method": "tools/call",
        "params": {"name": "get-time", "arguments": {}},
        "extra": "ignored",
    }))
    .expect("valid request");
    assert_eq!(decoded.seq, 3);
    assert_eq!(decoded.id, json!("rpc-1"));
    assert_eq!(decoded.method, "tools/call");
    assert_eq!(decoded.params["name"], json!("get-time"));
}

#[test]
fn wire_decode_rejects_malformed_methods() {
    for method in [
        json!(""),
        json!("bad method"),
        json!("line\nbreak"),
        json!(7),
    ] {
        assert!(
            PendingAppRequest::from_wire(json!({"seq": 1, "id": 1, "method": method})).is_none(),
            "{method}"
        );
    }
    assert!(PendingAppRequest::from_wire(json!({"seq": "1", "method": "tools/call"})).is_none());
}

#[test]
fn queue_is_bounded_and_fifo() {
    let mut queue = AppRequestQueue::new(2);
    assert!(queue.enqueue(request(1)));
    assert!(queue.enqueue(request(2)));
    assert!(!queue.enqueue(request(3)));
    assert_eq!(queue.dropped(), 1);
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.take_next().map(|request| request.seq), Some(1));
    assert_eq!(queue.take_next().map(|request| request.seq), Some(2));
    assert!(queue.take_next().is_none());
}

#[test]
fn queue_capacity_zero_is_clamped_to_one() {
    let mut queue = AppRequestQueue::new(0);
    assert!(queue.enqueue(request(1)));
    assert!(!queue.enqueue(request(2)));
    assert_eq!(queue.len(), 1);
}
