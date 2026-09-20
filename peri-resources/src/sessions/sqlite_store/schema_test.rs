use super::*;
use peri_acp_types::{
    messages::BaseMessage,
    store::{serialize_persisted_payload, PersistedPayload, ThreadStore},
    thread::ThreadMeta,
    workspace::{ScopedThreadQuery, ThreadScope},
};
use sqlx::{sqlite::SqliteConnectOptions, Connection};
use std::path::Path;

/// [回归测试] 实际旧库包含 thread_goals，不能因额外业务表而拒绝启动。
#[tokio::test]
async fn test_legacy_with_goals_upgrades_and_preserves_auxiliary_data() {
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
    let before = history_bytes(&mut connection).await;
    let auxiliary_query =
        "SELECT json_array(thread_id, goal_id, objective, status, token_budget, tokens_used,
        time_used_seconds, created_at_ms, updated_at_ms) FROM thread_goals";
    let goals: (String,) = sqlx::query_as(AssertSqlSafe(auxiliary_query))
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let extra_schema: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_schema WHERE name IN ('thread_goals', 'extension_state', 'idx_threads_parent_thread_id') ORDER BY name",
    ).fetch_all(&mut connection).await.unwrap();
    connection.close().await.unwrap();
    // 调用应用启动所用的 Resources 门面，复现相同的写打开入口。
    let resources = crate::Resources::open_with(Some(path.clone()))
        .await
        .unwrap();
    let store = resources.thread_store();
    assert_eq!(
        store
            .load_meta(&"old-session".to_owned())
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("旧会话")
    );
    assert!(store
        .load_session_binding(&"old-session".to_owned())
        .await
        .unwrap()
        .is_none());
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let after_goals: (String,) = sqlx::query_as(AssertSqlSafe(auxiliary_query))
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(after_goals, goals);
    let after_schema: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_schema WHERE name IN ('thread_goals', 'extension_state', 'idx_threads_parent_thread_id') ORDER BY name",
    ).fetch_all(&mut connection).await.unwrap();
    assert_eq!(after_schema, extra_schema);
    let (value,): (String,) =
        sqlx::query_as("SELECT value FROM extension_state WHERE key = 'state'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(value, "preserved extension bytes");
    assert_eq!(history_bytes(&mut connection).await, before);
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert!(violations.is_empty());
    connection.close().await.unwrap();
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    let id = store
        .create_bound_thread(ThreadMeta::new(dir.path().to_str().unwrap()), &workspace)
        .await
        .unwrap();
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    store
        .append_messages(&id, &[BaseMessage::human("新会话")])
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
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

/// [回归测试] 默认读写必须沿用原数据库，结构升级不能丢失历史或自动推断旧归属。
#[tokio::test]
async fn test_single_database_upgrade_preserves_history_and_binds_only_new_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    legacy_database(&path).await.close().await.unwrap();
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let old_id = "old-session".to_owned();
    let old = store.load_meta(&old_id).await.unwrap();
    assert_eq!(old.title.as_deref(), Some("旧会话"));
    assert_eq!(old.cwd, "/old/worktree");
    assert_eq!(old.message_count, 1);
    assert_eq!(
        store.load_messages(&old_id).await.unwrap()[0].content(),
        "保留的历史消息"
    );
    assert_eq!(store.load_session_binding(&old_id).await.unwrap(), None);
    assert!(matches!(
        store
            .acquire_execution_lease(&old_id)
            .await
            .err()
            .unwrap()
            .downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::BindingMissing)
    ));
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    let id = store
        .create_bound_thread(ThreadMeta::new(dir.path().to_str().unwrap()), &workspace)
        .await
        .unwrap();
    let lease = store.acquire_execution_lease(&id).await.unwrap();
    store
        .append_messages(&id, &[BaseMessage::human("新会话")])
        .await
        .unwrap();
    lease.mark_clean().await.unwrap();
    store.close().await;
    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    let page = reopened
        .list_scoped_threads(&ScopedThreadQuery {
            scope: ThreadScope::All,
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 2);
    assert!(page
        .entries
        .iter()
        .any(|entry| entry.thread.id == id && entry.binding.is_some()));
    assert!(page
        .entries
        .iter()
        .any(|entry| entry.thread.id == old_id && entry.binding.is_none()));
    assert_eq!(
        reopened.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    assert_eq!(reopened.load_meta(&old_id).await.unwrap().cwd, old.cwd);
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
    assert_eq!(version, 6);
    reopened.close().await;
    let reader = SqliteThreadStore::open_existing_read_only(&path)
        .await
        .unwrap();
    assert_eq!(reader.load_meta(&old_id).await.unwrap().title, old.title);
    reader.close().await;
    assert!(!dir.path().join("threads-v2.db").exists());
}

#[tokio::test]
async fn test_single_database_upgrade_preserves_all_existing_columns_and_context_bytes() {
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
        UPDATE threads SET parent_thread_id = 'old-parent', snapshot_at_message_id = 'snapshot',
            hidden = 1, cancel_policy = 'detach', config = 'config bytes', cached_context = 'cache bytes',
            frozen_context = 'frozen bytes', inherited_context = 'inherited bytes', agent_status = 'done',
            context_cache_epoch = 7;
        UPDATE messages SET truncated = 1, excluded = 1, projection = 'projection bytes';",
    ).execute(&mut connection).await.unwrap();
    let before = history_bytes(&mut connection).await;
    connection.close().await.unwrap();
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let after = history_bytes(&mut store.pool.acquire().await.unwrap()).await;
    assert_eq!(
        after, before,
        "所有原始列值（包括不解码的上下文）必须保持原样"
    );
    assert_eq!(
        store
            .load_session_binding(&"old-session".to_owned())
            .await
            .unwrap(),
        None
    );
    store.close().await;
}

async fn history_bytes(connection: &mut SqliteConnection) -> (String, String) {
    let (thread,): (String,) = sqlx::query_as(
        "SELECT json_array(id, title, cwd, created_at, updated_at, message_count, parent_thread_id,
            snapshot_at_message_id, hidden, cancel_policy, config, cached_context, frozen_context,
            inherited_context, agent_status, context_cache_epoch) FROM threads WHERE id = 'old-session'",
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
        error.to_string().contains("projects already exists"),
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
    sqlx::query("PRAGMA user_version = 7")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(matches!(
        error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::UnsupportedSchemaVersion {
            found: 7,
            supported: CURRENT_SCHEMA_VERSION,
        })
    ));
    // 拒绝理由必须可追溯：报错要复述实际版本与本构建上限，否则用户只知道「不支持」。
    let message = error.to_string();
    assert!(message.contains("version 7"), "{message}");
    assert!(
        message.contains(&format!("newest supported: {CURRENT_SCHEMA_VERSION}")),
        "{message}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!path.with_extension("db-wal").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_single_database_concurrent_upgrade_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    legacy_database(&path).await.close().await.unwrap();
    let (first, second) =
        tokio::join!(SqliteThreadStore::new(&path), SqliteThreadStore::new(&path));
    let first = first.unwrap();
    let second = second.unwrap();
    for store in [&first, &second] {
        assert_eq!(
            store
                .load_messages(&"old-session".to_owned())
                .await
                .unwrap()
                .len(),
            1
        );
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_bindings")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        store.close().await;
    }
}

/// [回归测试] Unix 子进程通过 HOME 隔离默认路径；Windows home_dir 不读取该环境变量。
#[cfg(unix)]
#[tokio::test]
async fn test_single_database_default_writer_upgrades_existing_default_path() {
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
    let store = SqliteThreadStore::new(&path).await.unwrap();
    assert_eq!(
        store
            .load_messages(&"old-session".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    store.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_single_database_default_writer_child_process() {
    let Ok(home) = std::env::var("PERI_TEST_SINGLE_DB_HOME") else {
        return;
    };
    let store = SqliteThreadStore::default_path().await.unwrap();
    assert_eq!(
        store.db_path,
        std::fs::canonicalize(Path::new(&home).join(".peri/threads/threads.db")).unwrap()
    );
    assert_eq!(
        store
            .load_meta(&"old-session".to_owned())
            .await
            .unwrap()
            .cwd,
        "/old/worktree"
    );
    store.close().await;
    let reader = crate::sessions::open_thread_store_read_only(None)
        .await
        .unwrap();
    assert_eq!(
        reader
            .load_meta(&"old-session".to_owned())
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("旧会话")
    );
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
async fn test_schema3_identity_migration_reuses_binding_and_survives_reopen() {
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

    let store = SqliteThreadStore::new(&path).await.unwrap();
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    assert_eq!(
        workspace.project_id.to_string(),
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(
        workspace.workspace_id.to_string(),
        "22222222-2222-4222-8222-222222222222"
    );
    let execution: (i64, bool) = sqlx::query_as(
        "SELECT generation, clean FROM execution_runs WHERE thread_id = 'old-session'",
    )
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(execution, (7, false));
    assert_eq!(
        store
            .load_messages(&"old-session".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .load_frozen_snapshot(&"old-session".to_owned())
            .await
            .unwrap()
            .as_deref(),
        Some("frozen-owner-state")
    );
    let binding = store
        .load_session_binding(&"old-session".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        binding.project_id.to_string(),
        workspace.project_id.to_string()
    );
    assert_eq!(
        store
            .validate_session_binding(&"old-session".to_owned())
            .await
            .unwrap(),
        workspace
    );
    store.close().await;

    let reopened = SqliteThreadStore::new(&path).await.unwrap();
    let again = reopened.resolve_workspace(dir.path()).await.unwrap();
    assert_eq!(again.project_id, workspace.project_id);
    assert_eq!(again.workspace_id, workspace.workspace_id);
    assert_eq!(
        reopened
            .load_session_binding(&"old-session".to_owned())
            .await
            .unwrap()
            .unwrap()
            .workspace_id,
        workspace.workspace_id
    );
    reopened.close().await;
}

/// 记录 v2 升级不能改动的身份与执行数据。
async fn identity_and_execution_bytes(connection: &mut SqliteConnection) -> Vec<String> {
    let mut values = Vec::new();
    for query in [
        "SELECT json_array(id, locator, object_identity) FROM projects ORDER BY id",
        "SELECT json_array(id, project_id, root, root_identity, discovery) FROM workspaces ORDER BY id",
        "SELECT json_array(thread_id, schema_version, project_id, workspace_id, relative_cwd) FROM session_bindings ORDER BY thread_id",
        "SELECT json_array(thread_id, generation, clean) FROM execution_runs ORDER BY thread_id",
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
async fn test_version2_upgrade_removes_required_revision_and_preserves_execution_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    let revision: (i64, Option<String>) = sqlx::query_as(
        "SELECT \"notnull\", dflt_value FROM pragma_table_info('session_bindings') WHERE name = 'revision'",
    ).fetch_one(&mut connection).await.unwrap();
    assert_eq!(revision, (1, None));
    let before_history = history_bytes(&mut connection).await;
    connection.close().await.unwrap();

    let store = SqliteThreadStore::new(&path).await.unwrap();
    let mut connection = store.pool.acquire().await.unwrap();
    assert_eq!(history_bytes(&mut connection).await, before_history);
    let migrated_identity = identity_and_execution_bytes(&mut connection).await;
    assert!(migrated_identity
        .iter()
        .all(|value| !value.contains("birth_")));
    assert!(!column_names(&mut connection, "session_bindings")
        .await
        .unwrap()
        .contains("revision"));
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(version, 6);
    drop(connection);

    let old = store
        .load_session_binding(&"old-session".to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.revision, 1);
    assert_eq!(
        old.project_id.to_string(),
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(
        old.workspace_id.to_string(),
        "22222222-2222-4222-8222-222222222222"
    );
    assert!(old.cwd_relative_to_workspace.as_os_str().is_empty());
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    let id = store
        .create_bound_thread(ThreadMeta::new(dir.path().to_str().unwrap()), &workspace)
        .await
        .unwrap();
    let owner = store.acquire_execution_lease(&id).await.unwrap();
    assert_eq!(
        store.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    assert_eq!(
        store
            .load_session_binding(&id)
            .await
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    owner.mark_clean().await.unwrap();
    store.close().await;
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
    let before_data = identity_and_execution_bytes(&mut connection).await;
    connection.close().await.unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    let message = error.to_string();
    assert!(
        message.contains("binding_revision_view") && message.contains("no such column: revision"),
        "应因视图依赖 revision 而拒绝 DROP COLUMN，实际错误：{message}"
    );
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
    assert_eq!(
        identity_and_execution_bytes(&mut connection).await,
        before_data
    );
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

/// 现行 schema 4：身份载荷已规范化，登记表仍保留单列唯一约束。
async fn version4_database(path: &Path, root: &Path) -> SqliteConnection {
    let mut connection = version3_database(path, root).await;
    let (identity,): (String,) = sqlx::query_as("SELECT object_identity FROM projects")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let identity =
        super::super::discovery::normalize_identity_json(&serde_json::from_str(&identity).unwrap())
            .unwrap();
    sqlx::query("UPDATE projects SET object_identity = ?")
        .bind(identity)
        .execute(&mut connection)
        .await
        .unwrap();
    let (root_identity, discovery): (String, String) =
        sqlx::query_as("SELECT root_identity, discovery FROM workspaces")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let root_identity = super::super::discovery::normalize_identity_json(
        &serde_json::from_str(&root_identity).unwrap(),
    )
    .unwrap();
    let discovery = super::super::discovery::normalize_discovery_json(
        &serde_json::from_str(&discovery).unwrap(),
    )
    .unwrap();
    sqlx::query("UPDATE workspaces SET root_identity = ?, discovery = ?")
        .bind(root_identity)
        .bind(discovery)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA user_version = 4")
        .execute(&mut connection)
        .await
        .unwrap();
    connection
}

/// [回归测试] schema 4 的单列唯一约束把「同一路径上的另一个文件对象」挡在登记之外，
/// 目录被替换或换位后该路径无法建立新会话。升级只把登记键放宽为组合键：行、绑定、
/// 外键、线程行与消息（含 frozen snapshot）都保持原样，同一定位 + 同一证据仍然唯一。
#[tokio::test]
async fn test_version4_upgrade_relaxes_registration_keys_and_preserves_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let status = std::process::Command::new("git")
        .args(["init", "-q", dir.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "Git fixture initialization failed");
    let mut connection = version4_database(&path, dir.path()).await;
    // 升级前：同一 root 上的第二个文件对象无法登记。
    let blocked = sqlx::query(
        "INSERT INTO workspaces (id, project_id, root, root_identity, discovery)
         SELECT '33333333-3333-4333-8333-333333333333', project_id, root, '{\"device\":9,\"inode\":9}', discovery
         FROM workspaces",
    )
    .execute(&mut connection)
    .await;
    assert!(blocked.is_err(), "schema 4 的单列唯一约束必须仍然存在");
    let before = identity_and_execution_bytes(&mut connection).await;
    let before_history = history_bytes(&mut connection).await;
    connection.close().await.unwrap();

    let store = SqliteThreadStore::new(&path).await.unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(version, 6);

    // 原有登记、绑定与历史原样可用。
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    assert_eq!(
        workspace.project_id.to_string(),
        "11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(
        workspace.workspace_id.to_string(),
        "22222222-2222-4222-8222-222222222222"
    );
    assert_eq!(
        store
            .validate_session_binding(&"old-session".to_owned())
            .await
            .unwrap(),
        workspace
    );
    assert_eq!(
        store
            .load_messages(&"old-session".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .load_frozen_snapshot(&"old-session".to_owned())
            .await
            .unwrap()
            .as_deref(),
        Some("frozen-owner-state"),
        "迁移不得丢失 frozen snapshot"
    );

    // 同一路径上的另一个文件对象可以登记，同一 (locator, 证据) 组合仍然唯一。
    let mut probe = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(&path).read_only(true),
    )
    .await
    .unwrap();
    let after = identity_and_execution_bytes(&mut probe).await;
    let after_history = history_bytes(&mut probe).await;
    probe.close().await.unwrap();
    assert_eq!(before, after, "迁移不得改写登记、绑定或执行状态");
    assert_eq!(
        before_history, after_history,
        "迁移不得改写线程行与消息：frozen snapshot 与历史都在其中"
    );

    sqlx::query(
        "INSERT INTO workspaces (id, project_id, root, root_identity, discovery)
         SELECT '33333333-3333-4333-8333-333333333333', project_id, root, '{\"device\":9,\"inode\":9}', discovery
         FROM workspaces WHERE id = '22222222-2222-4222-8222-222222222222'",
    )
    .execute(&store.pool)
    .await
    .unwrap();
    let duplicate_workspace = sqlx::query(
        "INSERT INTO workspaces (id, project_id, root, root_identity, discovery)
         SELECT '44444444-4444-4444-8444-444444444444', project_id, root, root_identity, discovery
         FROM workspaces WHERE id = '22222222-2222-4222-8222-222222222222'",
    )
    .execute(&store.pool)
    .await;
    assert!(
        duplicate_workspace.is_err(),
        "同一 (root, root_identity) 不得重复登记"
    );
    // 同一 locator 上的另一个文件对象可以登记（同一路径重新克隆）。
    sqlx::query(
        "INSERT INTO projects (id, locator, object_identity)
         SELECT '55555555-5555-4555-8555-555555555555', locator, '{\"device\":10,\"inode\":10}' FROM projects",
    )
    .execute(&store.pool)
    .await
    .unwrap();
    let duplicate_project = sqlx::query(
        "INSERT INTO projects (id, locator, object_identity)
         SELECT '66666666-6666-4666-8666-666666666666', locator, object_identity FROM projects
         WHERE id = '11111111-1111-4111-8111-111111111111'",
    )
    .execute(&store.pool)
    .await;
    assert!(
        duplicate_project.is_err(),
        "同一 (locator, object_identity) 不得重复登记"
    );
    // 重建登记表不能丢外键：引用不存在项目的工作区仍被拒绝。
    let orphan = sqlx::query(
        "INSERT INTO workspaces (id, project_id, root, root_identity, discovery)
         SELECT '77777777-7777-4777-8777-777777777777', 'missing-project', root || '-orphan', root_identity, discovery
         FROM workspaces WHERE id = '22222222-2222-4222-8222-222222222222'",
    )
    .execute(&store.pool)
    .await;
    assert!(orphan.is_err(), "工作区必须仍受 projects 外键约束");
    store.close().await;
}

/// 不可访问的旧会话也必须能升级；迁移不得为了补登记约束发现旧目录。
async fn assert_registration_upgrade_allows_directory_changes(version: i64) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = version2_database(&path).await;
    if version >= 3 {
        sqlx::query("ALTER TABLE session_bindings DROP COLUMN revision")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("PRAGMA user_version = 3")
            .execute(&mut connection)
            .await
            .unwrap();
    }
    if version >= 4 {
        // 独立重现旧版 3→4 的输出；5 的缺陷形态正是只更新版本号、未迁移约束。
        sqlx::raw_sql(
            "UPDATE projects SET object_identity = json_remove(object_identity, '$.birth_seconds', '$.birth_nanos');
             UPDATE workspaces SET root_identity = json_remove(root_identity, '$.birth_seconds', '$.birth_nanos'),
                discovery = json_remove(discovery, '$.root_identity.birth_seconds', '$.root_identity.birth_nanos');"
        ).execute(&mut connection).await.unwrap();
        sqlx::query(if version == 4 {
            "PRAGMA user_version = 4"
        } else {
            "PRAGMA user_version = 5"
        })
        .execute(&mut connection)
        .await
        .unwrap();
    }
    let old_history = history_bytes(&mut connection).await;
    let old_identity = identity_and_execution_bytes(&mut connection).await;
    connection.close().await.unwrap();
    let store = SqliteThreadStore::new(&path).await.unwrap();
    let original = dir.path().join("original");
    std::fs::create_dir(&original).unwrap();
    let workspace = store.resolve_workspace(&original).await.unwrap();
    let thread = store
        .create_bound_thread(ThreadMeta::new(original.to_str().unwrap()), &workspace)
        .await
        .unwrap();
    let binding = store.load_session_binding(&thread).await.unwrap();
    store.close().await;
    // 跨越关闭/重开；移动保留旧文件对象，再在原位置创建新对象，不依赖 inode 复用时序。
    let moved = dir.path().join("moved");
    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    let store = SqliteThreadStore::new(&path).await.unwrap();
    for cwd in [&moved, &original] {
        let resolved = store.resolve_workspace(cwd).await.unwrap_or_else(|error| {
            panic!("schema {version} 升级后目录 {cwd:?} 必须可登记：{error}")
        });
        assert_ne!(resolved.workspace_id, workspace.workspace_id);
        assert_ne!(resolved.project_id, workspace.project_id);
        let new_thread = store
            .create_bound_thread(ThreadMeta::new(cwd.to_str().unwrap()), &resolved)
            .await
            .unwrap();
        let owner = store.acquire_execution_lease(&new_thread).await.unwrap();
        assert_eq!(
            store.validate_session_binding(&new_thread).await.unwrap(),
            resolved
        );
        owner.mark_clean().await.unwrap();
    }
    assert_eq!(store.load_session_binding(&thread).await.unwrap(), binding);
    let error = store.validate_session_binding(&thread).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::NeedsRelink)
        ),
        "原路径被替换后旧会话必须拒绝执行：{error}"
    );
    let mut connection = store.pool.acquire().await.unwrap();
    // 精确读取旧会话，不以新增线程的插入顺序推断目标。
    assert_eq!(history_bytes(&mut connection).await, old_history);
    let after = identity_and_execution_bytes(&mut connection).await;
    assert!(
        old_identity[2..].iter().all(|row| after.contains(row)),
        "原 binding 和 dirty generation 不得被迁移清除或改写"
    );
    if version >= 4 {
        assert!(
            old_identity.iter().all(|row| after.contains(row)),
            "已规范化的登记证据必须原样保留"
        );
    }
    let (current,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(current, 6);
    drop(connection);
    store.close().await;
}

#[tokio::test]
async fn test_registration_upgrade_from_v2_allows_moved_and_replaced_directories() {
    assert_registration_upgrade_allows_directory_changes(2).await;
}

#[tokio::test]
async fn test_registration_upgrade_from_v3_allows_moved_and_replaced_directories() {
    assert_registration_upgrade_allows_directory_changes(3).await;
}

#[tokio::test]
async fn test_registration_upgrade_from_v4_allows_moved_and_replaced_directories() {
    assert_registration_upgrade_allows_directory_changes(4).await;
}

/// [回归测试] 已被旧 writer 标记为 5 的漏迁移库，重启后也必须自动补齐约束。
#[tokio::test]
async fn test_registration_upgrade_repairs_incomplete_v5() {
    assert_registration_upgrade_allows_directory_changes(5).await;
}

/// 独立构造健康 schema 5 的登记表；不能由本轮迁移生成 fixture，否则会掩盖回归。
async fn healthy_version5_database(path: &Path) -> SqliteConnection {
    let mut connection = version2_database(path).await;
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF;
         ALTER TABLE session_bindings DROP COLUMN revision;
         CREATE TABLE projects_v5 (
             id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
             UNIQUE(locator, object_identity));
         INSERT INTO projects_v5 SELECT id, locator, json_remove(object_identity, '$.birth_seconds', '$.birth_nanos') FROM projects;
         DROP TABLE projects;
         ALTER TABLE projects_v5 RENAME TO projects;
         CREATE TABLE workspaces_v5 (
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
             root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
             UNIQUE(root, root_identity), UNIQUE(id, project_id));
         INSERT INTO workspaces_v5 SELECT id, project_id, root,
             json_remove(root_identity, '$.birth_seconds', '$.birth_nanos'),
             json_remove(discovery, '$.root_identity.birth_seconds', '$.root_identity.birth_nanos') FROM workspaces;
         DROP TABLE workspaces;
         ALTER TABLE workspaces_v5 RENAME TO workspaces;
         PRAGMA user_version = 5;
         PRAGMA foreign_keys = ON;"
    ).execute(&mut connection).await.unwrap();
    connection
}

/// [回归测试] 健康 5 已允许同路径多对象、同对象多路径，补迁移不能重新收紧或归并它们。
#[tokio::test]
async fn test_registration_upgrade_preserves_healthy_v5_composite_registrations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = healthy_version5_database(&path).await;
    sqlx::raw_sql(
        "INSERT INTO projects SELECT '33333333-3333-4333-8333-333333333333', locator || '-moved', object_identity FROM projects;
         INSERT INTO projects SELECT '44444444-4444-4444-8444-444444444444', locator, '{\"device\":9,\"inode\":9}'
             FROM projects WHERE id = '11111111-1111-4111-8111-111111111111';
         INSERT INTO workspaces SELECT '55555555-5555-4555-8555-555555555555', project_id, root || '-moved', root_identity, discovery FROM workspaces;
         INSERT INTO workspaces SELECT '66666666-6666-4666-8666-666666666666', project_id, root, '{\"device\":9,\"inode\":9}', discovery
             FROM workspaces WHERE id = '22222222-2222-4222-8222-222222222222';"
    ).execute(&mut connection).await.unwrap();
    let before = identity_and_execution_bytes(&mut connection).await;
    let history = history_bytes(&mut connection).await;
    connection.close().await.unwrap();
    // 首次开库升级，第二次开库保持同一结果；不访问 fixture 中不存在的旧目录。
    for _ in 0..2 {
        let store = SqliteThreadStore::new(&path).await.unwrap();
        let mut connection = store.pool.acquire().await.unwrap();
        assert_eq!(identity_and_execution_bytes(&mut connection).await, before);
        assert_eq!(history_bytes(&mut connection).await, history);
        let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(version, 6);
        drop(connection);
        store.close().await;
    }
}

/// [回归测试] 补迁移遇到损坏引用必须回滚，不能留下半张表、升级版本或清 dirty。
#[tokio::test]
async fn test_registration_upgrade_corrupt_v5_rolls_back_all_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    let mut connection = healthy_version5_database(&path).await;
    sqlx::raw_sql(
        "PRAGMA foreign_keys = OFF;
         UPDATE session_bindings SET workspace_id = 'missing-workspace';
         PRAGMA foreign_keys = ON;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    let rows = identity_and_execution_bytes(&mut connection).await;
    let history = history_bytes(&mut connection).await;
    let schema: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    connection.close().await.unwrap();
    let error = SqliteThreadStore::new(&path).await.err().unwrap();
    assert!(
        matches!(error.downcast_ref::<WorkspaceError>(),
        Some(WorkspaceError::DiscoveryError(message)) if message == "registration rebuild broke references"),
        "悬空引用必须拒绝提交：{error}"
    );
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
    assert_eq!(after_schema, schema);
    assert_eq!(identity_and_execution_bytes(&mut connection).await, rows);
    assert_eq!(history_bytes(&mut connection).await, history);
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(version, 5, "失败不得提交新版本号");
    connection.close().await.unwrap();
}
