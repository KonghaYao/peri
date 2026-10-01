use super::*;
use crate::sessions::canonical::OAUTH_CREDENTIALS_TABLE;
use crate::sessions::data::SessionDataPort;
use crate::sessions::sqlite_store::{session_data::SqliteSessionData, SqliteThreadStore};

async fn fixture() -> (tempfile::TempDir, SqliteThreadStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteThreadStore::new(directory.path().join("sessions.db"))
        .await
        .unwrap();
    (directory, store)
}

fn scoped(store: &SqliteThreadStore, principal: &str, machine: &str) -> SqliteOAuthCredentialStore {
    SqliteOAuthCredentialStore::with_scope(store.database.clone(), principal, machine)
}

#[tokio::test]
async fn trait_crud_survives_session_database_restart() {
    let (directory, store) = fixture().await;
    let port = Arc::new(SqliteSessionData::new(store.database.clone()))
        .oauth_credentials()
        .unwrap();
    assert!(port.load("server").await.unwrap().is_none());
    port.save("server", r#"{"token":"synthetic-fixture"}"#)
        .await
        .unwrap();
    port.save("server", r#"{"token":"replacement-fixture"}"#)
        .await
        .unwrap();
    assert_eq!(port.list().await.unwrap(), vec!["server"]);
    let row: (String, String, String) =
        sqlx::query_as("SELECT principal_id, machine_id, updated_at FROM mcp_oauth_credentials")
            .fetch_one(&store.database.pool)
            .await
            .unwrap();
    assert_eq!(row.0, "local");
    assert_eq!(row.1, crate::sessions::machine::current().unwrap());
    assert!(chrono::DateTime::parse_from_rfc3339(&row.2).is_ok());
    store.close().await;
    let reopened = SqliteThreadStore::new(directory.path().join("sessions.db"))
        .await
        .unwrap();
    let port = Arc::new(SqliteSessionData::new(reopened.database.clone()))
        .oauth_credentials()
        .unwrap();
    assert_eq!(
        port.load("server").await.unwrap().as_deref(),
        Some(r#"{"token":"replacement-fixture"}"#)
    );
    port.clear("server").await.unwrap();
    port.clear("server").await.unwrap();
    assert!(port.load("server").await.unwrap().is_none());
    reopened.close().await;
}

#[tokio::test]
async fn all_operations_bind_principal_and_machine_scope() {
    let (_directory, store) = fixture().await;
    let own = scoped(&store, "principal", "machine");
    let other_principal = scoped(&store, "other", "machine");
    let other_machine = scoped(&store, "principal", "other");
    for port in [&other_principal, &other_machine] {
        port.save("server", r#"{"fixture":1}"#).await.unwrap();
        port.save("foreign-only", "{}").await.unwrap();
    }
    assert!(own.load("server").await.unwrap().is_none());
    assert!(own.list().await.unwrap().is_empty());
    own.save("server", "{}").await.unwrap();
    own.save("another", "{}").await.unwrap();
    assert_eq!(own.list().await.unwrap(), vec!["another", "server"]);
    own.clear("server").await.unwrap();
    own.clear_all().await.unwrap();
    own.clear_all().await.unwrap();
    assert!(own.list().await.unwrap().is_empty());
    for port in [&other_principal, &other_machine] {
        assert_eq!(
            port.load("server").await.unwrap().as_deref(),
            Some(r#"{"fixture":1}"#)
        );
        assert_eq!(port.list().await.unwrap(), vec!["foreign-only", "server"]);
    }
    store.close().await;
}

#[tokio::test]
async fn invalid_input_does_not_overwrite_credentials() {
    let (_directory, store) = fixture().await;
    let port = scoped(&store, "principal", "machine");
    port.save("server", "{}").await.unwrap();
    for payload in ["", "not-json", "{", "{} trailing", "null", "[]"] {
        assert!(matches!(
            port.save("server", payload).await,
            Err(OAuthCredentialError::InvalidData)
        ));
    }
    for key in ["".to_owned(), " \n".into(), "x".repeat(4097)] {
        assert!(matches!(
            port.save(&key, "{}").await,
            Err(OAuthCredentialError::InvalidInput)
        ));
        assert!(matches!(
            port.load(&key).await,
            Err(OAuthCredentialError::InvalidInput)
        ));
        assert!(matches!(
            port.clear(&key).await,
            Err(OAuthCredentialError::InvalidInput)
        ));
    }
    let maximum_key = "x".repeat(4096);
    let oversized_payload = format!(r#"{{"fixture":"{}"}}"#, "x".repeat(1024 * 1024));
    assert!(matches!(
        port.save("server", &oversized_payload).await,
        Err(OAuthCredentialError::InvalidInput)
    ));
    port.save(&maximum_key, "{}").await.unwrap();
    port.clear(&maximum_key).await.unwrap();
    assert_eq!(port.load("server").await.unwrap().as_deref(), Some("{}"));
    assert_eq!(port.list().await.unwrap(), vec!["server"]);
    store.close().await;
}

#[tokio::test]
async fn corrupt_records_and_sql_failures_are_redacted() {
    let (_directory, store) = fixture().await;
    let port = scoped(&store, "principal", "machine");
    sqlx::query("INSERT INTO mcp_oauth_credentials VALUES (?, ?, ?, ?, ?)")
        .bind("principal")
        .bind("machine")
        .bind("server")
        .bind("synthetic-sensitive-invalid-payload")
        .bind("fixture-time")
        .execute(&store.database.pool)
        .await
        .unwrap();
    let error = port.load("server").await.unwrap_err();
    assert!(matches!(error, OAuthCredentialError::InvalidData));
    assert_eq!(error.to_string(), "OAuth credential record is invalid");
    sqlx::query("DROP TABLE mcp_oauth_credentials")
        .execute(&store.database.pool)
        .await
        .unwrap();
    let error = port
        .save("synthetic-sensitive-key", "{}")
        .await
        .unwrap_err();
    assert!(matches!(error, OAuthCredentialError::Unavailable));
    assert_eq!(error.to_string(), "OAuth credential storage is unavailable");
    assert_eq!(format!("{error:?}"), "Unavailable");
    store.close().await;
}

#[tokio::test]
async fn readonly_provider_reads_and_rejects_every_mutation() {
    let (directory, store) = fixture().await;
    scoped(&store, "principal", "machine")
        .save("server", "{}")
        .await
        .unwrap();
    store.close().await;
    let readonly = SqliteThreadStore::open_existing_read_only(directory.path().join("sessions.db"))
        .await
        .unwrap();
    let port = scoped(&readonly, "principal", "machine");
    assert_eq!(port.load("server").await.unwrap().as_deref(), Some("{}"));
    assert_eq!(port.list().await.unwrap(), vec!["server"]);
    assert!(matches!(
        port.save("server", "{}").await,
        Err(OAuthCredentialError::ReadOnly)
    ));
    assert!(matches!(
        port.clear("server").await,
        Err(OAuthCredentialError::ReadOnly)
    ));
    assert!(matches!(
        port.clear_all().await,
        Err(OAuthCredentialError::ReadOnly)
    ));
    readonly.close().await;
}

#[tokio::test]
async fn closed_shared_pool_rejects_every_operation() {
    let (_directory, store) = fixture().await;
    let port = scoped(&store, "principal", "machine");
    store.close().await;
    assert!(matches!(
        port.load("server").await,
        Err(OAuthCredentialError::Unavailable)
    ));
    assert!(matches!(
        port.save("server", "{}").await,
        Err(OAuthCredentialError::Unavailable)
    ));
    assert!(matches!(
        port.clear("server").await,
        Err(OAuthCredentialError::Unavailable)
    ));
    assert!(matches!(
        port.clear_all().await,
        Err(OAuthCredentialError::Unavailable)
    ));
    assert!(matches!(
        port.list().await,
        Err(OAuthCredentialError::Unavailable)
    ));
}

#[tokio::test]
async fn current_schema_adds_only_oauth_table_without_version_change_or_json_import() {
    let (directory, store) = fixture().await;
    sqlx::query("DROP TABLE mcp_oauth_credentials")
        .execute(&store.database.pool)
        .await
        .unwrap();
    let before: Vec<(String, String)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
            .fetch_all(&store.database.pool)
            .await
            .unwrap();
    let version: (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(
        version.0,
        crate::sessions::sqlite_store::schema::CURRENT_SCHEMA_VERSION
    );
    store.close().await;
    let legacy = r#"{"server":{"token":"synthetic-legacy-fixture"}}"#;
    tokio::fs::write(directory.path().join("oauth_tokens.json"), legacy)
        .await
        .unwrap();
    let reopened = SqliteThreadStore::new(directory.path().join("sessions.db"))
        .await
        .unwrap();
    let port = scoped(
        &reopened,
        "local",
        crate::sessions::machine::current().unwrap(),
    );
    assert!(port.list().await.unwrap().is_empty());
    let after: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_schema WHERE sql IS NOT NULL AND name != ? ORDER BY name",
    )
    .bind(OAUTH_CREDENTIALS_TABLE)
    .fetch_all(&reopened.database.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    let after_version: (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(version, after_version);
    assert_eq!(
        tokio::fs::read_to_string(directory.path().join("oauth_tokens.json"))
            .await
            .unwrap(),
        legacy
    );
    reopened.close().await;
}

#[tokio::test]
async fn older_supported_schema_also_creates_oauth_table() {
    let (directory, store) = fixture().await;
    sqlx::query("DROP TABLE mcp_oauth_credentials")
        .execute(&store.database.pool)
        .await
        .unwrap();
    sqlx::query("PRAGMA user_version = 9")
        .execute(&store.database.pool)
        .await
        .unwrap();
    store.close().await;
    let reopened = SqliteThreadStore::new(directory.path().join("sessions.db"))
        .await
        .unwrap();
    let port = scoped(&reopened, "principal", "machine");
    port.save("server", "{}").await.unwrap();
    assert_eq!(port.load("server").await.unwrap().as_deref(), Some("{}"));
    reopened.close().await;
}
