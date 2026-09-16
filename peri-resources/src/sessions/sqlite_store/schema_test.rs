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
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].thread.id, id);
    assert_eq!(
        reopened.validate_session_binding(&id).await.unwrap(),
        workspace
    );
    assert_eq!(reopened.load_meta(&old_id).await.unwrap().cwd, old.cwd);
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
    assert_eq!(version, 3);
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
            inherited_context, agent_status, context_cache_epoch) FROM threads",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    let (message,): (String,) = sqlx::query_as(
        "SELECT json_array(message_id, thread_id, role, content, truncated, excluded, projection) FROM messages",
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
    sqlx::query("PRAGMA user_version = 4")
        .execute(&mut connection)
        .await
        .unwrap();
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
        INSERT INTO projects VALUES ('11111111-1111-4111-8111-111111111111', '/old/project', 'project identity bytes');
        INSERT INTO workspaces VALUES ('22222222-2222-4222-8222-222222222222',
            '11111111-1111-4111-8111-111111111111', '/old/worktree', 'root identity bytes', 'discovery bytes');
        INSERT INTO session_bindings VALUES ('old-session', 1, 1,
            '11111111-1111-4111-8111-111111111111', '22222222-2222-4222-8222-222222222222', '');
        INSERT INTO execution_runs VALUES ('old-session', 7, 0);
        PRAGMA user_version = 2;",
    ).execute(&mut connection).await.unwrap();
    connection
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
    let before_identity = identity_and_execution_bytes(&mut connection).await;
    connection.close().await.unwrap();

    let store = SqliteThreadStore::new(&path).await.unwrap();
    let mut connection = store.pool.acquire().await.unwrap();
    assert_eq!(history_bytes(&mut connection).await, before_history);
    assert_eq!(
        identity_and_execution_bytes(&mut connection).await,
        before_identity
    );
    assert!(!column_names(&mut connection, "session_bindings")
        .await
        .unwrap()
        .contains("revision"));
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(version, 3);
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
