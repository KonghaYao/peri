use crate::types::session::SessionBody;
use crate::types::IngestionEvent;

#[test]
fn test_session_create_serde_roundtrip() {
    let body = SessionBody {
        id: "sess_abc".to_string(),
        user_id: Some("user_1".to_string()),
        metadata: Some(serde_json::json!({"key": "value"})),
        release: Some("v1.0".to_string()),
        version: None,
        source: None,
        timestamp: None,
    };
    let json = serde_json::to_string(&body).unwrap();
    let de: SessionBody = serde_json::from_str(&json).unwrap();
    assert_eq!(de.id, "sess_abc");
    assert_eq!(de.user_id.as_deref(), Some("user_1"));
}

#[test]
fn test_session_create_in_ingestion_event() {
    let body = SessionBody {
        id: "sess_abc".to_string(),
        user_id: None,
        metadata: None,
        release: None,
        version: None,
        source: None,
        timestamp: None,
    };
    let event = IngestionEvent::SessionCreate {
        id: "evt_1".to_string(),
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        body,
        metadata: None,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("session-create"));
}

#[test]
fn test_session_update_in_ingestion_event() {
    let body = SessionBody {
        id: "sess_abc".to_string(),
        user_id: Some("user_2".to_string()),
        metadata: None,
        release: None,
        version: None,
        source: None,
        timestamp: None,
    };
    let event = IngestionEvent::SessionUpdate {
        id: "evt_2".to_string(),
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        body,
        metadata: None,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("session-update"));
}
