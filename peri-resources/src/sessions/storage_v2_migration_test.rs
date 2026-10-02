use super::*;
use sqlx::sqlite::SqliteConnectOptions;

async fn old_database() -> (tempfile::TempDir, std::path::PathBuf, String) {
    crate::sessions::machine::initialize().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let worktree = directory.path().join("project");
    std::fs::create_dir(&worktree).unwrap();
    let mut connection = connect(&path).await;
    for statement in canonical::CREATE_TABLES
        .iter()
        .chain(canonical::CREATE_INDEXES)
    {
        sqlx::query(*statement)
            .execute(&mut connection)
            .await
            .unwrap();
    }
    sqlx::query("PRAGMA user_version = 11")
        .execute(&mut connection)
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let project = uuid::Uuid::new_v4().to_string();
    let registration = uuid::Uuid::new_v4().to_string();
    let root = worktree.to_str().unwrap();
    let identity = r#"{"device":1,"inode":1}"#;
    let discovery = serde_json::json!({
        "root": root,
        "root_identity": {"device":1,"inode":1},
        "common_dir": null,
        "common_identity": null,
        "private_dir": null,
        "private_identity": null
    })
    .to_string();
    sqlx::query("INSERT INTO projects VALUES (?1, ?2, ?3)")
        .bind(&project)
        .bind(root)
        .bind(identity)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspaces VALUES (?1, ?2, ?3, ?4, ?5)")
        .bind(&registration)
        .bind(&project)
        .bind(root)
        .bind(identity)
        .bind(discovery)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO threads(id, cwd, created_at, updated_at) VALUES (?1, ?2, 'now', 'now')",
    )
    .bind(&id)
    .bind(root)
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query("INSERT INTO session_bindings VALUES (?1, 1, ?2, ?3, '')")
        .bind(&id)
        .bind(&project)
        .bind(&registration)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO session_environments(thread_id, machine_id) VALUES (?1, ?2)")
        .bind(&id)
        .bind(crate::sessions::machine::current().unwrap())
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO mcp_oauth_credentials VALUES ('local', ?1, 'server', 'old token', 'now')",
    )
    .bind(crate::sessions::machine::current().unwrap())
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    (directory, path, id)
}

async fn connect(path: &std::path::Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn local_migration_preserves_session_and_execution_registration() {
    let (_directory, path, id) = old_database().await;
    let mut connection = connect(&path).await;
    migrate_local_v2(&mut connection).await.unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 12);
    let row: (String, i64, String, String) = sqlx::query_as(
        "SELECT t.cwd, t.archived, w.path, w.machine_id
         FROM threads t JOIN workspaces w ON w.id = t.workspace_id WHERE t.id = ?1",
    )
    .bind(&id)
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(row.0, row.2);
    assert_eq!(row.1, 0);
    assert_eq!(row.3, crate::sessions::machine::current().unwrap());
    let (snapshot,): (String,) =
        sqlx::query_as("SELECT discovery_snapshot FROM session_bindings WHERE thread_id = ?1")
            .bind(&id)
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert!(snapshot.contains("root_identity"));
    let (old_registration,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM legacy_execution_registrations")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(old_registration, 1);
    let (credentials,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM mcp_oauth_credentials")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(credentials, 0);
}

#[tokio::test]
async fn late_migration_failure_keeps_v11_tables_and_credentials() {
    let (_directory, path, id) = old_database().await;
    let mut connection = connect(&path).await;
    sqlx::query("ALTER TABLE threads ADD COLUMN extension_value TEXT")
        .execute(&mut connection)
        .await
        .unwrap();
    assert!(migrate_local_v2(&mut connection).await.is_err());
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 11);
    let (old_table,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='workspaces'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(old_table, 1);
    let (session,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads WHERE id = ?1")
        .bind(&id)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(session, 1);
    let (credentials,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM mcp_oauth_credentials")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(credentials, 1);
}

#[tokio::test]
async fn replaced_directory_keeps_two_execution_snapshots_under_one_workspace() {
    let (_directory, path, first_id) = old_database().await;
    let mut connection = connect(&path).await;
    let second_id = uuid::Uuid::new_v4().to_string();
    let second_registration = uuid::Uuid::new_v4().to_string();
    let (project, root): (String, String) =
        sqlx::query_as("SELECT project_id, root FROM workspaces LIMIT 1")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let second_identity = r#"{"device":1,"inode":2}"#;
    let second_discovery = serde_json::json!({
        "root": root,
        "root_identity": {"device":1,"inode":2},
        "common_dir": null,
        "common_identity": null,
        "private_dir": null,
        "private_identity": null
    })
    .to_string();
    sqlx::query("INSERT INTO workspaces VALUES (?1, ?2, ?3, ?4, ?5)")
        .bind(&second_registration)
        .bind(&project)
        .bind(&root)
        .bind(second_identity)
        .bind(second_discovery)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO threads(id, cwd, created_at, updated_at) VALUES (?1, ?2, 'now', 'now')",
    )
    .bind(&second_id)
    .bind(&root)
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query("INSERT INTO session_bindings VALUES (?1, 1, ?2, ?3, '')")
        .bind(&second_id)
        .bind(&project)
        .bind(&second_registration)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO session_environments VALUES (?1, ?2)")
        .bind(&second_id)
        .bind(crate::sessions::machine::current().unwrap())
        .execute(&mut connection)
        .await
        .unwrap();
    migrate_local_v2(&mut connection).await.unwrap();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT t.workspace_id, b.discovery_snapshot FROM threads t
         JOIN session_bindings b ON b.thread_id = t.id
         WHERE t.id IN (?1, ?2) ORDER BY t.id",
    )
    .bind(&first_id)
    .bind(&second_id)
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, rows[1].0);
    assert_ne!(rows[0].1, rows[1].1);
    let (registrations,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM legacy_execution_registrations")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(registrations, 2);
}
