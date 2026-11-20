use super::{column_names, SqliteConnection, SqliteThreadStore, CURRENT_SCHEMA_VERSION};

// `SessionResources` 的方法只在 unix 子进程用例里调用（Windows 的 home_dir 不读 HOME）。
use peri_acp_types::messages::BaseMessage;
#[cfg(unix)]
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::store::ThreadStore;
use peri_acp_types::thread::ThreadMeta;
use peri_acp_types::workspace::ScopedThreadQuery;
use peri_acp_types::workspace::ThreadScope;
use peri_acp_types::workspace::WorkspaceError;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::AssertSqlSafe;
use sqlx::Connection;
use std::path::Path;

/// 已退役的运行时状态表：任何一个都不该出现在当前形状的库里。
const RECOVERY_TABLES: &[&str] = &[
    "session_work_commands",
    "session_work_state",
    "session_work_events",
    "session_work_receipts",
    "session_control_state",
    "session_control_receipts",
];

async fn assert_no_recovery_tables(pool: &sqlx::SqlitePool) {
    for table in RECOVERY_TABLES {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1")
            .bind(table)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[tokio::test]
async fn current_store_has_nine_business_tables_and_normal_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let store = SqliteThreadStore::try_new(&path).await.unwrap();
    assert_no_recovery_tables(&store.database.pool).await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    // v15 起九张 canonical 表：两张 inherited 派生索引表退役，执行登记并入 `workspaces`。
    // 没有登记表，也没有运行时状态表。
    assert_eq!(count, 9);
    let registrations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'legacy_execution_registrations'",
    )
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    assert_eq!(registrations, 0);
    let id = store
        .create_thread(ThreadMeta::new_at(
            directory.path().to_string_lossy().into_owned(),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    store
        .append_message(&id, BaseMessage::human("first"))
        .await
        .unwrap();
    store.close().await;
    let reopened = SqliteThreadStore::try_new(&path).await.unwrap();
    assert_eq!(
        reopened.load_context(&id).await.unwrap()[0].content(),
        "first"
    );
    reopened
        .append_message(&id, BaseMessage::human("next"))
        .await
        .unwrap();
    let history = reopened.load_context(&id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].content(), "next");
    reopened.close().await;
}

/// 压缩掉的开发期代数（16..19）一律拒绝：不迁移、不改写，库留在原位由上一版二进制打开。
/// 15 是当前代数，不在拒绝面里。
#[tokio::test]
async fn compressed_dev_generations_are_refused_without_touching_the_database() {
    for version in 16..=19_i64 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("threads.db");
        let store = SqliteThreadStore::try_new(&path).await.unwrap();
        let id = store
            .create_thread(ThreadMeta::new_at(
                directory.path().to_string_lossy().into_owned(),
                peri_time::now_wall(),
            ))
            .await
            .unwrap();
        store
            .append_message(&id, BaseMessage::human("preserved"))
            .await
            .unwrap();
        sqlx::query(AssertSqlSafe(format!("PRAGMA user_version = {version}")))
            .execute(&store.database.pool)
            .await
            .unwrap();
        let before: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
                .fetch_all(&store.database.pool)
                .await
                .unwrap();
        store.close().await;

        let error = match SqliteThreadStore::try_new(&path).await {
            Ok(_) => panic!("unexpected upgrade of schema {version}"),
            Err(error) => error,
        };
        assert!(matches!(
            error.downcast_ref::<WorkspaceError>(),
            Some(WorkspaceError::UnsupportedSchemaVersion { found, supported })
                if *found == version && *supported == CURRENT_SCHEMA_VERSION
        ));

        let mut connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new().filename(&path).read_only(true),
        )
        .await
        .unwrap();
        let after: Vec<(String, String, Option<String>)> =
            sqlx::query_as("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
                .fetch_all(&mut connection)
                .await
                .unwrap();
        assert_eq!(after, before, "{version}");
        let (stored,): (i64,) = sqlx::query_as("PRAGMA user_version")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(stored, version);
    }
}

/// [回归测试] 实际旧库包含 thread_goals，不能因额外业务表而拒绝启动。
///
/// 夹具里带一条 v9 代的退役索引 `idx_threads_parent_thread_id`：`threads` 在升级里被重建，
/// 重建表上的索引以 canonical 声明为准（见 `schema_shape` 的边界说明），因此它随重建消失，
/// 由这条测试显式记录，而不是靠实现的副作用。
#[tokio::test]
async fn test_legacy_with_goals_upgrades_preserving_goals_and_extensions() {
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
    let before_extension: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema WHERE name = 'extension_state'")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    connection.close().await.unwrap();
    // 调用应用启动所用的 Resources 门面，复现相同的写打开入口。
    let (store, _facade) = crate::sessions::open_store_and_facade_for_tests(path.clone())
        .await
        .unwrap();
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
    let (goals,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM sqlite_schema WHERE name = 'thread_goals'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(goals, 1);
    let after_extension: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT name, sql FROM sqlite_schema WHERE name = 'extension_state'")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(after_extension, before_extension);
    // `threads` 被重建到 canonical 形状：旧代留下的退役索引随重建消失，索引集合以
    // canonical 的声明为准（迁移末尾整表重放一次）。索引是派生对象，不承载数据。
    let (retired,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'idx_threads_parent_thread_id'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(retired, 0);
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
        .create_bound_thread(
            ThreadMeta::new_at(dir.path().to_str().unwrap(), peri_time::now_wall()),
            &workspace,
        )
        .await
        .unwrap();
    store
        .append_messages(&id, &[BaseMessage::human("新会话")])
        .await
        .unwrap();
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
    let error = SqliteThreadStore::try_new(&path).await.err().unwrap();
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
        .bind(
            peri_acp_types::store::serialize_persisted_payload(&PersistedPayload::Message(message))
                .unwrap(),
        )
        .execute(&mut connection)
        .await
        .unwrap();
    connection
}

/// [回归测试] 默认读写沿用原数据库，升级保留历史、不补造旧绑定，执行代际独立记录。
#[tokio::test]
async fn test_single_database_upgrade_preserves_history_and_binds_only_new_sessions() {
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
    let history: Vec<(String, String)> = sqlx::query_as(
        "SELECT message_id, content FROM messages WHERE thread_id = 'old-session' ORDER BY rowid",
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    let store = SqliteThreadStore::try_new(&path).await.unwrap();
    let old_id = "old-session".to_owned();
    let old = store.load_meta(&old_id).await.unwrap();
    assert_eq!(old.title.as_deref(), Some("旧会话"));
    assert_eq!(old.cwd, "/old/worktree");
    assert_eq!(old.message_count, 1);
    assert_eq!(old.config.as_deref(), Some(r#"{"model":"legacy"}"#));
    assert_eq!(
        store.load_messages(&old_id).await.unwrap()[0].content(),
        "保留的历史消息"
    );
    assert_eq!(store.load_session_binding(&old_id).await.unwrap(), None);
    assert_eq!(store.load_session_binding(&old_id).await.unwrap(), None);
    assert_eq!(store.load_frozen_snapshot(&old_id).await.unwrap(), None);
    let workspace = store.resolve_workspace(dir.path()).await.unwrap();
    let id = store
        .create_bound_thread(
            ThreadMeta::new_at(dir.path().to_str().unwrap(), peri_time::now_wall()),
            &workspace,
        )
        .await
        .unwrap();
    store
        .append_messages(&id, &[BaseMessage::human("新会话")])
        .await
        .unwrap();
    store.close().await;
    let reopened = SqliteThreadStore::try_new(&path).await.unwrap();
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
    assert_eq!(
        serde_json::to_value(reopened.load_meta(&old_id).await.unwrap()).unwrap(),
        serde_json::to_value(&old).unwrap()
    );
    let persisted_history: Vec<(String, String)> = sqlx::query_as(
        "SELECT message_id, content FROM messages WHERE thread_id = ?1 ORDER BY rowid",
    )
    .bind(&old_id)
    .fetch_all(&reopened.database.pool)
    .await
    .unwrap();
    assert_eq!(persisted_history, history);
    assert_eq!(reopened.load_session_binding(&old_id).await.unwrap(), None);
    assert_eq!(reopened.load_frozen_snapshot(&old_id).await.unwrap(), None);
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
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
        UPDATE threads SET parent_thread_id = NULL, snapshot_at_message_id = 'snapshot',
            hidden = 1, cancel_policy = 'detach', config = 'config bytes', cached_context = 'cache bytes',
            frozen_context = 'frozen bytes', inherited_context = 'inherited bytes', agent_status = 'done',
            context_cache_epoch = 7;
        UPDATE messages SET truncated = 1, excluded = 1, projection = 'projection bytes';",
    ).execute(&mut connection).await.unwrap();
    let before = history_bytes(&mut connection).await;
    connection.close().await.unwrap();
    let store = SqliteThreadStore::try_new(&path).await.unwrap();
    let after = history_bytes(&mut store.database.pool.acquire().await.unwrap()).await;
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
    let error = SqliteThreadStore::try_new(&path).await.err().unwrap();
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
    // 比本构建更新一代：写打开必须拒绝，且不猜列形状、不降级写。
    let future = CURRENT_SCHEMA_VERSION + 1;
    sqlx::query(AssertSqlSafe(format!("PRAGMA user_version = {future}")))
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = SqliteThreadStore::try_new(&path).await.err().unwrap();
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
async fn test_single_database_concurrent_upgrade_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("threads.db");
    legacy_database(&path).await.close().await.unwrap();
    let (first, second) = tokio::join!(
        SqliteThreadStore::try_new(&path),
        SqliteThreadStore::try_new(&path)
    );
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
            .fetch_one(&store.database.pool)
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
    let store = SqliteThreadStore::try_new(&path).await.unwrap();
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
        store.database.db_path,
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
    // 只读命令面（meta 子命令）的生产入口：只读门面，不创建目录/库/schema。
    let reader = crate::sessions::open_session_resources_read_only(None)
        .await
        .unwrap();
    assert_eq!(
        reader
            .load_session_meta(&"old-session".to_owned())
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("旧会话")
    );
}
include!("schema_identity_test.rs");

#[path = "schema_registration_test.rs"]
mod registration_tests;
