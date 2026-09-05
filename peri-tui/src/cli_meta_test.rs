use chrono::{TimeZone, Utc};
use peri_acp_types::thread::{AgentStatus, ThreadMeta};

use super::*;

fn meta_with_control_characters() -> ThreadMeta {
    ThreadMeta {
        id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
        title: Some("title\n\t\u{1b}[31m".to_owned()),
        cwd: "/tmp/project\rnext".to_owned(),
        created_at: Utc.with_ymd_and_hms(2026, 9, 4, 0, 0, 0).unwrap(),
        updated_at: Utc.with_ymd_and_hms(2026, 9, 4, 0, 10, 0).unwrap(),
        message_count: 12,
        content_size: 999,
        parent_thread_id: None,
        snapshot_at_message_id: Some("forbidden-snapshot".to_owned()),
        hidden: true,
        cancel_policy: Default::default(),
        config: Some("forbidden-config-secret".to_owned()),
        cached_context: Some("forbidden-cached-context".to_owned()),
        agent_status: AgentStatus::Done,
    }
}

#[test]
fn json_success_has_exact_nine_field_projection() {
    let outcome = success_outcome(SessionMetaDtoV1::from(meta_with_control_characters()), true);
    let value: serde_json::Value =
        serde_json::from_str(outcome.stdout.as_deref().unwrap()).unwrap();
    let object = value.as_object().unwrap();
    let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();

    assert_eq!(
        keys,
        [
            "createdAt",
            "cwd",
            "id",
            "messageCount",
            "parentThreadId",
            "persistedAgentStatus",
            "schemaVersion",
            "title",
            "updatedAt",
        ]
    );
    assert_eq!(object["schemaVersion"], 1);
    assert_eq!(object["messageCount"], 12);
    assert_eq!(object["parentThreadId"], serde_json::Value::Null);
    assert_eq!(object["persistedAgentStatus"], "done");
    assert_eq!(object["createdAt"], "2026-09-04T00:00:00+00:00");
    assert!(outcome.stderr.is_none());
    assert_eq!(outcome.exit_code, 0);
}

#[test]
fn human_success_preserves_unicode_and_escapes_stored_control_characters() {
    let mut meta = meta_with_control_characters();
    meta.title = Some("标题\n\t\u{1b}[31m".to_owned());
    meta.cwd = "/tmp/项目\rnext".to_owned();
    let outcome = success_outcome(SessionMetaDtoV1::from(meta), false);
    let output = outcome.stdout.unwrap();

    assert!(output.contains("Title: 标题\\n\\t\\u{1b}[31m"));
    assert!(output.contains("CWD: /tmp/项目\\rnext"));
    assert!(!output.contains("forbidden-config-secret"));
    assert!(!output.contains("forbidden-cached-context"));
    assert!(!output.contains("forbidden-snapshot"));
    assert!(outcome.stderr.is_none());
}

#[test]
fn json_errors_have_stable_shape_and_exit_mapping() {
    let cases = [
        (MetaErrorKind::InvalidSessionId, "invalid_session_id", 2),
        (MetaErrorKind::DatabaseNotFound, "database_not_found", 3),
        (MetaErrorKind::DatabaseUnreadable, "database_unreadable", 4),
        (MetaErrorKind::SchemaIncompatible, "schema_incompatible", 4),
        (MetaErrorKind::SessionNotFound, "session_not_found", 3),
        (MetaErrorKind::CorruptSessionData, "corrupt_session_data", 4),
        (MetaErrorKind::InternalError, "internal_error", 1),
    ];

    for (kind, expected_kind, expected_exit) in cases {
        let outcome = error_outcome(kind, true);
        let value: serde_json::Value =
            serde_json::from_str(outcome.stderr.as_deref().unwrap()).unwrap();
        assert!(outcome.stdout.is_none());
        assert_eq!(outcome.exit_code, expected_exit);
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["error"]["kind"], expected_kind);
        assert!(value["error"]["message"].is_string());
        assert_eq!(value.as_object().unwrap().len(), 2);
    }
}

#[test]
fn internal_error_human_contract_is_stable_without_product_trigger() {
    let outcome = error_outcome(MetaErrorKind::InternalError, false);

    assert!(outcome.stdout.is_none());
    assert_eq!(
        outcome.stderr.as_deref(),
        Some("internal_error: an internal error occurred\n")
    );
    assert_eq!(outcome.exit_code, 1);
}

#[tokio::test]
async fn invalid_uuid_fails_before_missing_database_is_observed() {
    let outcome = run_meta_session(
        Some(PathBuf::from("/definitely/missing/threads.db")),
        "not-a-uuid".to_owned(),
        true,
    )
    .await;
    let value: serde_json::Value =
        serde_json::from_str(outcome.stderr.as_deref().unwrap()).unwrap();

    assert_eq!(outcome.exit_code, 2);
    assert_eq!(value["error"]["kind"], "invalid_session_id");
}
