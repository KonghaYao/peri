use super::*;
use peri_acp_types::{
    messages::BaseMessage,
    store::{serialize_persisted_payload, PersistedPayload, ThreadStore},
};
use sqlx::{sqlite::SqliteConnectOptions, AssertSqlSafe, Connection};
use std::path::Path;

#[tokio::test]
async fn v13_refusal_preserves_owner_tables_and_close_intent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(LEGACY_V13_SQL)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO machines VALUES ('machine','legacy','known');
        INSERT INTO workspaces VALUES ('workspace','machine','/old','unverified');
        INSERT INTO threads(id,cwd,created_at,updated_at,workspace_id) VALUES ('session','/old','2026-10-05T00:00:00Z','2026-10-05T00:00:00Z','workspace');
        CREATE TABLE session_execution_owners(root_id TEXT PRIMARY KEY REFERENCES threads(id),epoch INTEGER NOT NULL,nonce TEXT NOT NULL,expires_at_unix INTEGER NOT NULL,released INTEGER NOT NULL);
        CREATE TABLE session_execution_workspace_descriptors(root_id TEXT PRIMARY KEY REFERENCES session_execution_owners(root_id),owner_epoch INTEGER NOT NULL,endpoint TEXT NOT NULL,key_identity TEXT NOT NULL,agent_generation_id TEXT NOT NULL,unsupported_async_owners INTEGER NOT NULL);
        INSERT INTO session_execution_owners VALUES ('session',7,'retired',0,0);
        INSERT INTO session_execution_workspace_descriptors VALUES ('session',7,'retired','retired','retired',0);
        INSERT INTO session_close_intents VALUES ('session','2026-10-05T00:00:00Z');
        PRAGMA user_version=13;").execute(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    assert_legacy_rejected(&path).await;
}

#[tokio::test]
async fn test_legacy_with_goals_refuses_and_preserves_goals_and_extensions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("fixtures/legacy_with_goals.sql"))
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count)
            VALUES ('old-session', '旧会话', '/old/worktree', '2026-09-01T00:00:00Z', '2026-09-02T00:00:00Z', 1);
        INSERT INTO messages (message_id, thread_id, role, content)
            VALUES ('old-message', 'old-session', 'user', 'original message bytes');
        INSERT INTO thread_goals VALUES ('old-session', 'goal-1', '保留目标', 'paused', 1000, 42, 9, 1, 2);
        CREATE TABLE extension_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        INSERT INTO extension_state VALUES ('state', 'preserved extension bytes');",
    ).execute(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    let error = crate::sessions::open_store_and_facade_for_tests(path.clone())
        .await
        .err()
        .expect("legacy facade must refuse");
    assert!(error
        .to_string()
        .contains("explicit stopped-writer offline migration"));
    assert_legacy_rejected(&path).await;
}

#[tokio::test]
async fn test_legacy_auxiliary_tables_do_not_allow_views_to_replace_required_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE auxiliary_state (id TEXT PRIMARY KEY);
        CREATE VIEW threads AS SELECT 'id' AS id, 'title' AS title, '/cwd' AS cwd,
            'created' AS created_at, 'updated' AS updated_at, 1 AS message_count;
        CREATE TABLE messages (message_id TEXT PRIMARY KEY, thread_id TEXT, role TEXT, content TEXT);",
    ).execute(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::UnsupportedDatabaseSchema)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!path.with_extension("db-wal").exists());
}

// 旧 writer 未设置 user_version；fixture 独立于新 schema 初始化代码。
async fn legacy_database(path: &Path) -> SqliteConnection {
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE threads (
            id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE messages (
            message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            role TEXT NOT NULL, content TEXT NOT NULL
        );
        INSERT INTO threads VALUES ('old-session', '旧会话', '/old/worktree',
            '2026-09-01T00:00:00Z', '2026-09-02T00:00:00Z', 1);",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let message = BaseMessage::human("保留的历史消息");
    sqlx::query("INSERT INTO messages VALUES (?, 'old-session', 'user', ?)")
        .bind(message.id().as_uuid().to_string())
        .bind(serialize_persisted_payload(&PersistedPayload::Message(message)).unwrap())
        .execute(&mut connection)
        .await
        .unwrap();
    connection
}

#[tokio::test]
async fn test_single_database_refusal_preserves_history_without_binding_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = legacy_database(&path).await;
    sqlx::raw_sql(
        "ALTER TABLE threads ADD COLUMN config TEXT;
        UPDATE threads SET config = '{\"model\":\"legacy\"}' WHERE id = 'old-session';",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    assert_legacy_rejected(&path).await;
}

#[tokio::test]
async fn test_single_database_refusal_preserves_all_existing_columns_and_context_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = legacy_database(&path).await;
    sqlx::raw_sql(
        "ALTER TABLE threads ADD COLUMN parent_thread_id TEXT;
        ALTER TABLE threads ADD COLUMN snapshot_at_message_id TEXT;
        ALTER TABLE threads ADD COLUMN hidden BOOLEAN NOT NULL DEFAULT 0;
        ALTER TABLE threads ADD COLUMN cancel_policy TEXT NOT NULL DEFAULT 'cascade';
        ALTER TABLE threads ADD COLUMN config TEXT;
        ALTER TABLE threads ADD COLUMN cached_context TEXT;
        ALTER TABLE threads ADD COLUMN frozen_context TEXT;
        ALTER TABLE threads ADD COLUMN inherited_context TEXT;
        ALTER TABLE threads ADD COLUMN agent_status TEXT NOT NULL DEFAULT 'active';
        ALTER TABLE threads ADD COLUMN context_cache_epoch INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN truncated BOOLEAN NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN excluded BOOLEAN NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN projection TEXT;
        UPDATE threads SET parent_thread_id = NULL, snapshot_at_message_id = 'snapshot',
            hidden = 1, cancel_policy = 'detach', config = 'config bytes', cached_context = 'cache bytes',
            frozen_context = 'frozen bytes', inherited_context = 'inherited bytes', agent_status = 'done',
            context_cache_epoch = 7;
        UPDATE messages SET truncated = 1, excluded = 1, projection = 'projection bytes';",
    ).execute(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    assert_legacy_rejected(&path).await;
}

async fn history_bytes(connection: &mut SqliteConnection) -> (String, String) {
    let (thread,): (String,) = sqlx::query_as(
        "SELECT json_array(id, title, cwd, created_at, updated_at, message_count, parent_thread_id,
            snapshot_at_message_id, hidden, cancel_policy, config, frozen_context,
            inherited_context, agent_status) FROM threads WHERE id = 'old-session'",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    let (message,): (String,) = sqlx::query_as(
        "SELECT json_array(message_id, thread_id, role, content, truncated, excluded, projection) FROM messages WHERE thread_id = 'old-session'",
    ).fetch_one(connection).await.unwrap();
    (thread, message)
}

#[tokio::test]
async fn test_single_database_failed_upgrade_rolls_back_schema_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = legacy_database(&path).await;
    // 名称冲突在旧列已添加后才触发错误，证明整次 DDL 升级会回滚。
    sqlx::query("CREATE VIEW projects AS SELECT id FROM threads")
        .execute(&mut connection)
        .await
        .unwrap();
    let before: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    connection.close().await.unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(
        error
            .to_string()
            .contains("explicit stopped-writer offline migration"),
        "{error}"
    );
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let after: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(after, before);
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 0);
    connection.close().await.unwrap();
}

#[tokio::test]
async fn test_single_database_future_version_is_rejected_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = legacy_database(&path).await;
    // 比本构建更新一代：写打开必须拒绝，且不猜列形状、不降级写。
    let future = CURRENT_SCHEMA_VERSION + 1;
    sqlx::query(AssertSqlSafe(format!("PRAGMA user_version = {future}")))
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::UnsupportedSchemaVersion {
            found,
            supported: CURRENT_SCHEMA_VERSION,
        }) if *found == future
    ));
    // 拒绝理由必须可追溯：报错要复述实际版本与本构建上限，否则用户只知道「不支持」。
    let message = error.to_string();
    assert!(message.contains(&format!("version {future}")), "{message}");
    assert!(
        message.contains(&format!("newest supported: {CURRENT_SCHEMA_VERSION}")),
        "{message}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!path.with_extension("db-wal").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_single_database_concurrent_legacy_refusal_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    legacy_database(&path).await.close().await.unwrap();
    let before = legacy_evidence(&path).await;
    let (first, second) =
        tokio::join!(SqliteThreadStore::new(&path), SqliteThreadStore::new(&path));
    for opened in [first, second] {
        let error = opened.err().expect("concurrent legacy opens must refuse");
        assert!(error
            .to_string()
            .contains("explicit stopped-writer offline migration"));
    }
    assert_eq!(legacy_evidence(&path).await, before);
    assert_legacy_rejected(&path).await;
}

/// [回归测试] Unix 子进程通过 HOME 隔离默认路径；Windows home_dir 不读取该环境变量。
#[cfg(unix)]
#[tokio::test]
async fn test_single_database_default_writer_refuses_existing_legacy_path() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join(".peri/threads");
    std::fs::create_dir_all(&parent).unwrap();
    let path = parent.join("threads.db");
    legacy_database(&path).await.close().await.unwrap();
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::sqlite_store::schema::tests::test_single_database_default_writer_child_process",
            "--nocapture",
        ])
        .env("HOME", dir.path())
        .env("USERPROFILE", dir.path())
        .env("PERI_TEST_SINGLE_DB_HOME", dir.path())
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
    assert!(!parent.join("threads-v2.db").exists());
    assert_legacy_rejected(&path).await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_single_database_default_writer_child_process() {
    let Ok(home) = std::env::var("PERI_TEST_SINGLE_DB_HOME") else {
        return;
    };
    let path = Path::new(&home).join(".peri/threads/threads.db");
    let before = legacy_evidence(&path).await;
    let error = SqliteThreadStore::default_path()
        .await
        .err()
        .expect("default writer must refuse old schema");
    assert!(error
        .to_string()
        .contains("explicit stopped-writer offline migration"));
    assert!(crate::sessions::open_session_resources_read_only(None)
        .await
        .is_err());
    assert_eq!(legacy_evidence(&path).await, before);
}

// 独立保留 v2 的 revision 非空且无默认值约束，避免新 schema 掩盖旧库 INSERT 失败。
async fn version2_database(path: &Path) -> SqliteConnection {
    let mut connection = legacy_database(path).await;
    sqlx::raw_sql(
        r#"ALTER TABLE threads ADD COLUMN parent_thread_id TEXT;
        ALTER TABLE threads ADD COLUMN snapshot_at_message_id TEXT;
        ALTER TABLE threads ADD COLUMN hidden BOOLEAN NOT NULL DEFAULT 0;
        ALTER TABLE threads ADD COLUMN cancel_policy TEXT NOT NULL DEFAULT 'cascade';
        ALTER TABLE threads ADD COLUMN config TEXT;
        ALTER TABLE threads ADD COLUMN cached_context TEXT;
        ALTER TABLE threads ADD COLUMN frozen_context TEXT;
        ALTER TABLE threads ADD COLUMN inherited_context TEXT;
        ALTER TABLE threads ADD COLUMN agent_status TEXT NOT NULL DEFAULT 'active';
        ALTER TABLE threads ADD COLUMN context_cache_epoch INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN truncated BOOLEAN NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN excluded BOOLEAN NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN projection TEXT;
        CREATE INDEX idx_messages_thread_id ON messages(thread_id);
        CREATE TABLE projects (
            id TEXT PRIMARY KEY, locator TEXT NOT NULL UNIQUE, object_identity TEXT NOT NULL UNIQUE
        );
        CREATE TABLE workspaces (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
            root TEXT NOT NULL UNIQUE, root_identity TEXT NOT NULL UNIQUE, discovery TEXT NOT NULL,
            UNIQUE(id, project_id)
        );
        CREATE TABLE session_bindings (
            thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
            schema_version INTEGER NOT NULL, revision INTEGER NOT NULL,
            project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
            FOREIGN KEY(workspace_id, project_id) REFERENCES workspaces(id, project_id)
        );
        CREATE INDEX idx_bindings_project ON session_bindings(project_id, thread_id);
        CREATE INDEX idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id);
        CREATE INDEX idx_threads_updated ON threads(updated_at DESC, id DESC) WHERE hidden = 0 AND message_count > 0;
        CREATE TABLE execution_runs (
            thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
            generation INTEGER NOT NULL, clean BOOLEAN NOT NULL
        );
        INSERT INTO projects VALUES ('11111111-1111-4111-8111-111111111111', '/old/project', '{"device":1,"inode":2,"birth_seconds":3,"birth_nanos":4}');
        INSERT INTO workspaces VALUES ('22222222-2222-4222-8222-222222222222',
            '11111111-1111-4111-8111-111111111111', '/old/worktree', '{"device":5,"inode":6,"birth_seconds":7,"birth_nanos":8}', '{"root":"/old/worktree","root_identity":{"device":5,"inode":6,"birth_seconds":7,"birth_nanos":8},"common_dir":null,"common_identity":null,"private_dir":null,"private_identity":null}');
        INSERT INTO session_bindings VALUES ('old-session', 1, 1,
            '11111111-1111-4111-8111-111111111111', '22222222-2222-4222-8222-222222222222', '');
        INSERT INTO execution_runs VALUES ('old-session', 7, 0);
        PRAGMA user_version = 2;"#,
    ).execute(&mut connection).await.unwrap();
    connection
}

// A real schema-3 registry: old identity payloads still carry birth fields,
// while all directory values point at the live temporary workspace.
async fn version3_database(path: &Path, root: &Path) -> SqliteConnection {
    let mut connection = version2_database(path).await;
    let (_, observed) = super::super::discovery::observe(root).await.unwrap();
    let discovery = observed.discovery;
    let identity_with_legacy_fields = |value: serde_json::Value| {
        let mut value = value;
        value["birth_seconds"] = serde_json::json!(123);
        value["birth_nanos"] = serde_json::json!(456);
        value
    };
    let project_identity =
        identity_with_legacy_fields(serde_json::to_value(discovery.project_identity()).unwrap());
    let mut discovery_json = serde_json::to_value(&discovery).unwrap();
    for key in ["root_identity", "common_identity", "private_identity"] {
        assert!(
            discovery_json[key].is_object(),
            "Git fixture must include {key}"
        );
        discovery_json[key] = identity_with_legacy_fields(discovery_json[key].clone());
    }
    let root_identity = discovery_json["root_identity"].clone();
    let root = discovery.root.to_str().unwrap();
    let locator = discovery.project_locator().to_str().unwrap();
    sqlx::query("UPDATE projects SET locator = ?, object_identity = ? WHERE id = ?")
        .bind(locator)
        .bind(serde_json::to_string(&project_identity).unwrap())
        .bind("11111111-1111-4111-8111-111111111111")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("UPDATE workspaces SET root = ?, root_identity = ?, discovery = ? WHERE id = ?")
        .bind(root)
        .bind(serde_json::to_string(&root_identity).unwrap())
        .bind(serde_json::to_string(&discovery_json).unwrap())
        .bind("22222222-2222-4222-8222-222222222222")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("UPDATE threads SET frozen_context = ? WHERE id = 'old-session'")
        .bind("frozen-owner-state")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE session_bindings DROP COLUMN revision")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA user_version = 3")
        .execute(&mut connection)
        .await
        .unwrap();
    connection
}

#[tokio::test]
async fn test_schema3_refusal_preserves_binding_and_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let status = std::process::Command::new("git")
        .args(["init", "-q", dir.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "Git fixture initialization failed");
    version3_database(&path, dir.path())
        .await
        .close()
        .await
        .unwrap();

    assert_legacy_rejected(&path).await;
}

/// 记录 v2 升级不能改动的身份数据。
async fn identity_bytes(connection: &mut SqliteConnection) -> Vec<String> {
    let mut values = Vec::new();
    let migrated: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'legacy_execution_registrations'",
    )
    .fetch_one(&mut *connection).await.unwrap();
    let registration_query = if migrated.0 == 0 {
        "SELECT json_array(id, project_id, root, root_identity, discovery) FROM workspaces ORDER BY id"
    } else {
        "SELECT json_array(id, project_id, root, root_identity, discovery) FROM legacy_execution_registrations ORDER BY id"
    };
    for query in [
        "SELECT json_array(id, locator, object_identity) FROM projects ORDER BY id",
        registration_query,
        "SELECT json_array(thread_id, schema_version, project_id, workspace_id, relative_cwd) FROM session_bindings ORDER BY thread_id",
    ] {
        let rows: Vec<(String,)> = sqlx::query_as(AssertSqlSafe(query))
            .fetch_all(&mut *connection)
            .await
            .unwrap();
        values.extend(rows.into_iter().map(|(value,)| value));
    }
    values
}

#[tokio::test]
async fn test_version2_refusal_preserves_required_revision_and_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    let revision: (i64, Option<String>) = sqlx::query_as(
        "SELECT \"notnull\", dflt_value FROM pragma_table_info('session_bindings') WHERE name = 'revision'",
    ).fetch_one(&mut connection).await.unwrap();
    assert_eq!(revision, (1, None));
    connection.close().await.unwrap();

    assert_legacy_rejected(&path).await;
}

#[tokio::test]
async fn test_version2_failed_column_drop_preserves_schema_version_and_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    // 现有视图依赖 revision 时，SQLite 必须拒绝删除该列。
    sqlx::query("CREATE VIEW binding_revision_view AS SELECT revision FROM session_bindings")
        .execute(&mut connection)
        .await
        .unwrap();
    let before_schema: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    let before_data = identity_bytes(&mut connection).await;
    connection.close().await.unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    let message = error.to_string();
    assert!(message.contains("explicit stopped-writer offline migration"));
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let after_schema: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(after_schema, before_schema);
    assert_eq!(identity_bytes(&mut connection).await, before_data);
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 2);
    connection.close().await.unwrap();
}

#[tokio::test]
async fn test_identity_migration_collision_rolls_back_schema_and_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    sqlx::query("INSERT INTO projects VALUES (?, ?, ?)")
        .bind("33333333-3333-4333-8333-333333333333")
        .bind("/another/project")
        .bind(r#"{"device":1,"inode":2,"birth_seconds":9,"birth_nanos":10}"#)
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    assert!(SqliteThreadStore::new(&path).await.is_err());
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 2, "冲突必须回滚版本号");
    let (identity,): (String,) = sqlx::query_as(
        "SELECT object_identity FROM projects WHERE id = '11111111-1111-4111-8111-111111111111'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert!(identity.contains("birth_seconds"), "回滚不得改写旧身份");
}

#[tokio::test]
async fn test_identity_migration_corrupt_discovery_rolls_back_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    sqlx::query("UPDATE workspaces SET discovery = 'corrupt'")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    assert!(SqliteThreadStore::new(&path).await.is_err());
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 2, "损坏 discovery 必须回滚版本号");
}

#[path = "schema_registration_test.rs"]
mod registration_tests;

async fn assert_legacy_rejected(path: &std::path::Path) {
    let before = legacy_evidence(path).await;
    let backup = path.with_extension("verified-backup");
    std::fs::copy(path, &backup).unwrap();
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        std::fs::read(path).unwrap()
    );
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    if version != 17 {
        let approval = crate::sessions::StoppedWriterApproval {
            source_version: version,
            writers_stopped: true,
            backup_verified: true,
        };
        assert!(crate::sessions::migrate_stopped_work_store(path, &approval)
            .await
            .is_err());
    }
    let error = SqliteThreadStore::new(path)
        .await
        .err()
        .expect("legacy writer must refuse");
    assert!(
        error
            .to_string()
            .contains("explicit stopped-writer offline migration"),
        "{error:#}"
    );
    let error = SqliteThreadStore::open_existing_read_only(path)
        .await
        .err()
        .expect("inline history is not reference history");
    assert!(matches!(
        error.kind(),
        crate::sessions::sqlite_store::connection::ReadOnlyStoreErrorKind::SchemaIncompatible
    ));
    assert_eq!(
        legacy_evidence(path).await,
        before,
        "refusal must preserve every schema object and row"
    );
}

const LEGACY_V13_SQL: &str = r#"CREATE TABLE IF NOT EXISTS machines (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    identity_kind TEXT NOT NULL CHECK(identity_kind IN ('known', 'legacy_unknown'))
);
CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    machine_id TEXT NOT NULL REFERENCES machines(id),
    path TEXT NOT NULL,
    path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
    UNIQUE(machine_id, path)
);
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))
);
CREATE TABLE IF NOT EXISTS messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT
);
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
    UNIQUE(locator, object_identity)
);
CREATE TABLE IF NOT EXISTS legacy_execution_registrations (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
    UNIQUE(root, root_identity), UNIQUE(id, project_id)
);
CREATE TABLE IF NOT EXISTS session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    discovery_snapshot TEXT,
    evidence_origin TEXT NOT NULL,
    FOREIGN KEY(workspace_id, project_id) REFERENCES legacy_execution_registrations(id, project_id)
);
CREATE TABLE IF NOT EXISTS mcp_oauth_credentials (
    principal_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    server_key TEXT NOT NULL,
    credentials_blob TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(principal_id, workspace_id, server_key)
);
CREATE TABLE IF NOT EXISTS session_close_intents (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    requested_at TEXT NOT NULL
);"#;
const LEGACY_V17_WORK_SQL: &str = r#"CREATE TABLE IF NOT EXISTS session_control_state (
    session_id TEXT PRIMARY KEY NOT NULL,
    state_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS session_control_receipts (
    command_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL,
    digest TEXT NOT NULL,
    resolution_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS session_work_state (session_id TEXT PRIMARY KEY NOT NULL, state_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS session_work_events (event_key TEXT PRIMARY KEY NOT NULL, event_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS session_work_receipts (mutation_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL, digest TEXT NOT NULL, resolution_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS session_work_commands (mutation_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL, digest TEXT NOT NULL, command_json TEXT NOT NULL, reconciled INTEGER NOT NULL DEFAULT 0 CHECK(reconciled IN (0,1)));
PRAGMA user_version=17;"#;

async fn legacy_work_database(path: &Path, valid: bool) -> String {
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(LEGACY_V13_SQL)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql(LEGACY_V17_WORK_SQL)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO machines VALUES ('machine','legacy','known');
        INSERT INTO workspaces VALUES ('workspace','machine','/old','unverified');
        INSERT INTO threads(id,cwd,created_at,updated_at,workspace_id,frozen_context,message_count) VALUES ('session','/old','2026-10-05T00:00:00Z','2026-10-05T00:00:00Z','workspace','frozen bytes',1);
        INSERT INTO session_close_intents VALUES ('session','original close timestamp');
        INSERT INTO session_control_state VALUES ('session','{\"lifecycle\":1,\"revision\":0,\"controlGeneration\":0,\"status\":\"closing\",\"attempt\":null}');
        INSERT INTO session_work_state VALUES ('session','{}');
        INSERT INTO session_work_commands VALUES ('original','session','digest','unknown original command bytes',0);
        INSERT INTO session_work_events VALUES ('event','original event bytes');").execute(&mut connection).await.unwrap();
    let message = BaseMessage::human("original history 🦀");
    let content = if valid {
        serialize_persisted_payload(&PersistedPayload::Message(message.clone())).unwrap()
    } else {
        "invalid original JSON".into()
    };
    sqlx::query("INSERT INTO messages(rowid,message_id,thread_id,role,content,truncated,excluded,projection) VALUES (7,?1,'session','user',?2,1,1,'original projection bytes')")
        .bind(message.id().as_uuid().to_string()).bind(&content).execute(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    content
}

fn stopped_approval(path: &Path) -> crate::sessions::StoppedWriterApproval {
    let backup = path.with_extension("verified-backup");
    std::fs::copy(path, &backup).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), std::fs::read(backup).unwrap());
    crate::sessions::StoppedWriterApproval {
        source_version: 17,
        writers_stopped: true,
        backup_verified: true,
    }
}

#[tokio::test]
async fn legacy_v17_open_refuses_until_public_stopped_migration_preserves_history_and_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy17.db");
    let content = legacy_work_database(&path, true).await;
    assert_legacy_rejected(&path).await;
    let approval = stopped_approval(&path);
    let report = crate::sessions::migrate_stopped_work_store(&path, &approval)
        .await
        .unwrap();
    assert_eq!(report.messages, 1);
    assert!(report.retained_evidence >= 3);
    assert!(report.legacy_unknown_sessions >= 1);
    for _ in 0..2 {
        let store = SqliteThreadStore::new(&path).await.unwrap();
        let row: (i64, bool, bool, String) =
            sqlx::query_as("SELECT rowid,truncated,excluded,projection FROM messages")
                .fetch_one(&store.database.pool)
                .await
                .unwrap();
        assert_eq!(row, (7, true, true, "original projection bytes".into()));
        let payload: Vec<u8> = sqlx::query_scalar("SELECT p.bytes FROM messages m JOIN session_payloads p ON p.storage_scope=m.thread_id AND p.payload_id=json_extract(m.content_ref,'$.payloadId')").fetch_one(&store.database.pool).await.unwrap();
        assert_eq!(payload, content.as_bytes());
        assert_eq!(
            store.load_messages(&"session".into()).await.unwrap()[0].content(),
            "original history 🦀"
        );
        let evidence: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT bytes FROM session_payloads WHERE kind='legacyOriginal'")
                .fetch_all(&store.database.pool)
                .await
                .unwrap();
        for expected in [
            "{}",
            "unknown original command bytes",
            "original event bytes",
        ] {
            assert!(evidence.iter().any(|bytes| bytes == expected.as_bytes()));
        }
        assert_eq!(
            store
                .load_frozen_snapshot(&"session".into())
                .await
                .unwrap()
                .as_deref(),
            Some("frozen bytes")
        );
        let close: String = sqlx::query_scalar(
            "SELECT requested_at FROM session_close_intents WHERE thread_id='session'",
        )
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
        assert_eq!(close, "original close timestamp");
        assert!(store
            .load_session_binding(&"session".into())
            .await
            .unwrap()
            .is_none());
        store.close().await;
        let reader = SqliteThreadStore::open_existing_read_only(&path)
            .await
            .unwrap();
        assert_eq!(
            reader
                .load_meta(&"session".into())
                .await
                .unwrap()
                .message_count,
            1
        );
        reader.close().await;
    }
}

#[tokio::test]
async fn public_stopped_migration_requires_backup_and_stopped_writers_without_writes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy17.db");
    legacy_work_database(&path, true).await;
    let before = std::fs::read(&path).unwrap();
    for (writers_stopped, backup_verified) in [(false, true), (true, false), (false, false)] {
        let approval = crate::sessions::StoppedWriterApproval {
            source_version: 17,
            writers_stopped,
            backup_verified,
        };
        assert!(
            crate::sessions::migrate_stopped_work_store(&path, &approval)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn failed_public_stopped_migration_preserves_original_schema_and_raw_rows() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy17.db");
    legacy_work_database(&path, false).await;
    let approval = stopped_approval(&path);
    let before = std::fs::read(&path).unwrap();
    assert!(
        crate::sessions::migrate_stopped_work_store(&path, &approval)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_legacy_rejected(&path).await;
}

async fn legacy_evidence(path: &std::path::Path) -> Vec<(String, Vec<String>)> {
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .unwrap();
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let schema: Vec<String> = sqlx::query_scalar("SELECT json_array(type,name,tbl_name,sql) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").fetch_all(&mut connection).await.unwrap();
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut connection).await.unwrap();
    let mut evidence = vec![
        ("version".into(), vec![version.to_string()]),
        ("schema".into(), schema),
    ];
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
                .bind(&table)
                .fetch_all(&mut connection)
                .await
                .unwrap();
        let expressions = columns
            .iter()
            .map(|column| format!("quote(\"{}\")", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" || ',' || ");
        let statement = format!(
            "SELECT quote(rowid) || ':' || {expressions} FROM \"{}\" ORDER BY rowid",
            table.replace('"', "\"\"")
        );
        let rows = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
            .fetch_all(&mut connection)
            .await
            .unwrap();
        evidence.push((table, rows));
    }
    connection.close().await.unwrap();
    evidence
}
