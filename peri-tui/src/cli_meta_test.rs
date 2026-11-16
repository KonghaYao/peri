use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use peri_acp_types::session_resources::AccessMode;
use peri_acp_types::session_store::SessionStoreDeployment;
use peri_acp_types::thread::{AgentStatus, ThreadMeta};

use super::MetaErrorKind;
use super::SessionMetaDtoV1;
use super::SessionResourceError;
use super::SessionResourceErrorKind;

#[test]
fn resource_failure_details_survive_meta_error_projection() {
    let message = "database https://example.test/db?token=fixture failed\ncaused by: TLS reset";
    let error = SessionResourceError::new(SessionResourceErrorKind::Unavailable {
        detail: message.to_owned(),
    });
    let outcome = super::error_outcome_with_message(
        super::map_resource_error(&error),
        true,
        error.to_string(),
    );
    assert_eq!(outcome.exit_code, 4);
    let wire: serde_json::Value = serde_json::from_str(outcome.stderr.as_deref().expect("resource failure details survive meta error projection: fixture must have the expected variant")).expect("resource failure details survive meta error projection: fixture JSON must match the expected schema");
    assert_eq!(wire["error"]["kind"], "database_unreadable");
    assert!(wire["error"]["message"].as_str().expect("resource failure details survive meta error projection: the fixture must retain its expected variant").contains(message));
}

fn meta_with_control_characters() -> ThreadMeta {
    ThreadMeta {
        id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
        title: Some("title\n\t\u{1b}[31m".to_owned()),
        cwd: "/tmp/project\rnext".to_owned(),
        created_at: Utc
            .with_ymd_and_hms(2026, 9, 4, 0, 0, 0)
            .single()
            .expect("meta with control characters: the fixed fixture date and time must be valid"),
        updated_at: Utc
            .with_ymd_and_hms(2026, 9, 4, 0, 10, 0)
            .single()
            .expect("meta with control characters: the fixed fixture date and time must be valid"),
        message_count: 12,
        content_size: 999,
        parent_thread_id: None,
        snapshot_at_message_id: Some("forbidden-snapshot".to_owned()),
        hidden: true,
        cancel_policy: Default::default(),
        config: Some("forbidden-config-secret".to_owned()),
        agent_status: AgentStatus::Done,
        work_state: Default::default(),
        context_usage_tokens: None,
    }
}

#[test]
fn json_success_has_exact_nine_field_projection() {
    let outcome =
        super::success_outcome(SessionMetaDtoV1::from(meta_with_control_characters()), true);
    let value: serde_json::Value = serde_json::from_str(outcome.stdout.as_deref().expect(
        "json success has exact nine field projection: fixture must have the expected variant",
    ))
    .expect(
        "json success has exact nine field projection: fixture JSON must match the expected schema",
    );
    let object = value.as_object().expect(
        "json success has exact nine field projection: fixture must have the expected variant",
    );
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
    let outcome = super::success_outcome(SessionMetaDtoV1::from(meta), false);
    let output = outcome.stdout.expect("human success preserves unicode and escapes stored control characters: the fixture process outcome must include the expected output stream");

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
        (MetaErrorKind::StoreNotConfigured, "store_not_configured", 2),
        (MetaErrorKind::StoreUnavailable, "store_unavailable", 4),
        (MetaErrorKind::InternalError, "internal_error", 1),
    ];

    for (kind, expected_kind, expected_exit) in cases {
        let outcome = super::error_outcome(kind, true);
        let value: serde_json::Value =
            serde_json::from_str(outcome.stderr.as_deref().expect("json errors have stable shape and exit mapping: fixture must have the expected variant")).expect("json errors have stable shape and exit mapping: fixture JSON must match the expected schema");
        assert!(outcome.stdout.is_none());
        assert_eq!(outcome.exit_code, expected_exit);
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["error"]["kind"], expected_kind);
        assert!(value["error"]["message"].is_string());
        assert_eq!(value.as_object().expect("json errors have stable shape and exit mapping: the fixture must retain its expected variant").len(), 2);
    }
}

#[test]
fn internal_error_human_contract_is_stable_without_product_trigger() {
    let outcome = super::error_outcome(MetaErrorKind::InternalError, false);

    assert!(outcome.stdout.is_none());
    assert_eq!(
        outcome.stderr.as_deref(),
        Some("internal_error: an internal error occurred\n")
    );
    assert_eq!(outcome.exit_code, 1);
}

#[tokio::test]
async fn invalid_uuid_fails_before_missing_database_is_observed() {
    let outcome = super::run_meta_session(
        SessionStoreDeployment::local_path(PathBuf::from("/definitely/missing/threads.db"))
            .with_access(AccessMode::ReadOnly),
        "not-a-uuid".to_owned(),
        true,
    )
    .await;
    let value: serde_json::Value =
        serde_json::from_str(outcome.stderr.as_deref().expect("invalid uuid fails before missing database is observed: fixture must have the expected variant")).expect("invalid uuid fails before missing database is observed: fixture JSON must match the expected schema");

    assert_eq!(outcome.exit_code, 2);
    assert_eq!(value["error"]["kind"], "invalid_session_id");
}

// ─── 统一只读入口的错误映射（D-04）───────────────────────────────────────────

const META_TEST_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

/// 显式不会存在的凭证变量名：用例自己保证它不存在，因此既不受开发者环境影响，
/// 也不会真的去打真实网络。
const ABSENT_CREDENTIAL_ENV: &str = "PERI_META_TEST_ABSENT_TOKEN_ENV";

/// 缺库仍是原来的 database_not_found（exit 3），不被新分类掩盖。
#[tokio::test]
async fn missing_database_maps_to_database_not_found() {
    let dir = tempfile::tempdir().expect("missing database maps to database not found: isolated fixture workspace must remain accessible");
    let deployment = SessionStoreDeployment::local_path(dir.path().join("missing.db"))
        .with_access(AccessMode::ReadOnly);
    let outcome = super::run_meta_session(deployment, META_TEST_UUID.to_owned(), true).await;
    let value: serde_json::Value = serde_json::from_str(outcome.stderr.as_deref().expect(
        "missing database maps to database not found: fixture must have the expected variant",
    ))
    .expect(
        "missing database maps to database not found: fixture JSON must match the expected schema",
    );

    assert_eq!(outcome.exit_code, 3);
    assert_eq!(value["error"]["kind"], "database_not_found");
}

/// 远程 locator 而凭证来源没配好（变量显式不存在）是**配置**错误：exit 2
/// `store_not_configured`，在连网与本机 I/O 之前失败；错误保留凭证来源名。
///
/// 该用例的前身是 `unwired_remote_store_reports_unavailable`（前提 `RemoteStoreNotWired`
/// 已在 C 批删除，远程分支是真装配）：此时再断言「远程不可用」既非事实，也要求真去连网。
#[tokio::test]
async fn missing_credential_configuration_is_a_configuration_error() {
    // 受控环境：显式移除变量，保证这条断言不依赖开发者环境、也不会打真实网络。
    if !isolated_environment("missing_credential_configuration_is_a_configuration_error") {
        return;
    }
    assert!(
        std::env::var_os(ABSENT_CREDENTIAL_ENV).is_none(),
        "凭证变量必须显式不存在: {ABSENT_CREDENTIAL_ENV}"
    );

    let outcome = super::run_meta_session(
        SessionStoreDeployment::from_locator("turso://sentinel-db-sentinel-org.turso.io")
            .with_credential_env(ABSENT_CREDENTIAL_ENV)
            .with_access(AccessMode::ReadOnly),
        META_TEST_UUID.to_owned(),
        true,
    )
    .await;
    let value: serde_json::Value =
        serde_json::from_str(outcome.stderr.as_deref().expect("missing credential configuration is a configuration error: fixture must have the expected variant")).expect("missing credential configuration is a configuration error: fixture JSON must match the expected schema");
    let rendered = outcome.stderr.as_deref().expect("missing credential configuration is a configuration error: fixture must have the expected variant");

    assert_eq!(outcome.exit_code, 2);
    assert_eq!(value["error"]["kind"], "store_not_configured");
    assert!(
        !rendered.contains("sentinel-db-sentinel-org"),
        "不回显 locator 原文: {rendered}"
    );
    assert!(rendered.contains(ABSENT_CREDENTIAL_ENV));
}

/// 缺少 locator 的凭证来源是配置错误：不是「空库」也不是「不存在」。
#[tokio::test]
async fn credential_without_locator_is_a_configuration_error() {
    let outcome = super::run_meta_session(
        SessionStoreDeployment::default_local()
            .with_credential_env(ABSENT_CREDENTIAL_ENV)
            .with_access(AccessMode::ReadOnly),
        META_TEST_UUID.to_owned(),
        false,
    )
    .await;

    assert_eq!(outcome.exit_code, 2);
    assert!(
        outcome
            .stderr
            .as_deref()
            .expect("credential without locator is a configuration error: the fixture process outcome must include the expected output stream")
            .contains("store_not_configured"),
        "{:?}",
        outcome.stderr
    );
}

/// Run only the selected environment fixture; verify that its assertion body ran.
fn isolated_environment(case: &str) -> bool {
    const CHILD: &str = "PERI_ENV_STYLE_FIXTURE";
    if std::env::var(CHILD).as_deref() == Ok(case) {
        return true;
    }
    let module = module_path!()
        .split_once("::")
        .expect("test module has a crate prefix")
        .1;
    let output = std::process::Command::new(
        std::env::current_exe().expect("the running test executable remains available"),
    )
    .args([
        "--exact",
        &format!("{module}::{case}"),
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD, case)
    .env_remove(ABSENT_CREDENTIAL_ENV)
    .output()
    .expect("the isolated environment fixture must launch");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the child must execute one fixture"
    );
    false
}
