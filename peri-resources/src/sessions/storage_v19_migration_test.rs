//! v18 → v19 迁移夹具：形状、回填、绑定收敛与失败回滚。
//!
//! 夹具**手写 v18 的 DDL**（不引用 `canonical` 的当前常量）：这些语句是历史形状的
//! 快照，升级实现改动时它们不该跟着漂移，否则「从上一版升级」这件事就没有被验证。

use super::*;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};

/// v18 的形状：`workspaces` 还没有证据列，绑定指向执行登记表。
const V18_DDL: &[&str] = &[
    "CREATE TABLE machines (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL CHECK(length(trim(name)) > 0),
        identity_kind TEXT NOT NULL CHECK(identity_kind IN ('known', 'legacy_unknown')))",
    "CREATE TABLE workspaces (
        id TEXT PRIMARY KEY,
        machine_id TEXT NOT NULL REFERENCES machines(id),
        path TEXT NOT NULL,
        path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
        UNIQUE(machine_id, path))",
    "CREATE TABLE threads (
        id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
        parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
        cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
        frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
        workspace_id TEXT NOT NULL REFERENCES workspaces(id),
        archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1)))",
    "CREATE TABLE messages (
        message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        role TEXT NOT NULL, content TEXT NOT NULL,
        truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT)",
    "CREATE TABLE projects (
        id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
        UNIQUE(locator, object_identity))",
    "CREATE TABLE legacy_execution_registrations (
        id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
        root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
        UNIQUE(root, root_identity), UNIQUE(id, project_id))",
    "CREATE TABLE session_bindings (
        thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
        schema_version INTEGER NOT NULL,
        project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
        discovery_snapshot TEXT, evidence_origin TEXT NOT NULL DEFAULT 'legacy_last_observation',
        FOREIGN KEY(workspace_id, project_id) REFERENCES \"legacy_execution_registrations\"(id, project_id))",
    "CREATE TABLE mcp_oauth_credentials (
        principal_id TEXT NOT NULL, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
        server_key TEXT NOT NULL, credentials_blob TEXT NOT NULL, updated_at TEXT NOT NULL,
        PRIMARY KEY (principal_id, workspace_id, server_key))",
    "CREATE TABLE session_environments (
        thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
        machine_id TEXT NOT NULL)",
    "CREATE INDEX idx_messages_thread_id ON messages(thread_id)",
    "CREATE INDEX idx_bindings_project ON session_bindings(project_id, thread_id)",
    "CREATE INDEX idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id)",
    "CREATE INDEX idx_threads_workspace_archived ON threads(workspace_id, archived, updated_at DESC, id DESC)",
];

/// v18 的会话关闭意图表：v18 批次补的，v12 形状里没有。
const CLOSE_INTENTS_DDL: &str = "CREATE TABLE session_close_intents (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    requested_at TEXT NOT NULL)";

const DISCOVERY: &str = r#"{"root":"R","root_identity":{"device":1,"inode":1},"common_dir":null,"common_identity":null,"private_dir":null,"private_identity":null}"#;

fn discovery_at(root: &str) -> String {
    DISCOVERY.replace("\"R\"", &format!("{root:?}"))
}

struct Fixture {
    /// 只用来保证库文件活到用例结束。
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
    machine: String,
}

impl Fixture {
    /// 开一个 v18 形状的库：`version` 取 12 时同时覆盖 v12 起点（缺少 v18 补的表）。
    async fn open(version: i64) -> Self {
        crate::sessions::machine::initialize().await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("threads.db");
        let mut connection = connect(&path).await;
        for statement in V18_DDL {
            // 语句文本来自本文件的静态清单，不含外部输入。
            sqlx::raw_sql(sqlx::AssertSqlSafe((*statement).to_owned()))
                .execute(&mut connection)
                .await
                .unwrap();
        }
        if version >= 18 {
            sqlx::query(CLOSE_INTENTS_DDL)
                .execute(&mut connection)
                .await
                .unwrap();
        }
        let machine = crate::sessions::machine::current().unwrap().to_owned();
        sqlx::query(
            "INSERT INTO machines(id, name, identity_kind) VALUES (?1, '我的电脑', 'known')",
        )
        .bind(&machine)
        .execute(&mut connection)
        .await
        .unwrap();
        // 版本号来自本文件的静态取值（12 / 18），不含外部输入。
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "PRAGMA user_version = {version}"
        )))
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        Self {
            _directory: directory,
            path,
            machine,
        }
    }

    async fn connection(&self) -> SqliteConnection {
        connect(&self.path).await
    }
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

/// 一条工作区记录：项目 + 归属行 + 执行登记。
async fn insert_workspace(
    connection: &mut SqliteConnection,
    project: &str,
    path: &str,
    owner_id: &str,
    registration_id: &str,
) {
    sqlx::query("INSERT INTO projects(id, locator, object_identity) VALUES (?1, ?2, ?3)")
        .bind(project)
        .bind(path)
        .bind(r#"{"device":1,"inode":1}"#)
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspaces(id, machine_id, path, path_source) VALUES (?1, ?2, ?3, 'discovered')")
        .bind(owner_id)
        .bind(crate::sessions::machine::current().unwrap())
        .bind(path)
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO legacy_execution_registrations(id, project_id, root, root_identity, discovery)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(registration_id)
    .bind(project)
    .bind(path)
    .bind(r#"{"device":1,"inode":1}"#)
    .bind(discovery_at(path))
    .execute(&mut *connection)
    .await
    .unwrap();
}

/// 一条会话行 + 一条绑定行（`binding_workspace_id` 是绑定自己记录的执行身份）。
async fn insert_session(
    connection: &mut SqliteConnection,
    thread: &str,
    path: &str,
    owner_id: &str,
    project: &str,
    binding_workspace_id: &str,
) {
    sqlx::query(
        "INSERT INTO threads(id, cwd, created_at, updated_at, workspace_id)
         VALUES (?1, ?2, '2026-10-08T00:00:00Z', '2026-10-08T00:00:00Z', ?3)",
    )
    .bind(thread)
    .bind(path)
    .bind(owner_id)
    .execute(&mut *connection)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO session_bindings(thread_id, schema_version, project_id, workspace_id, relative_cwd,
            discovery_snapshot, evidence_origin)
         VALUES (?1, 1, ?2, ?3, '', ?4, 'creation_snapshot')",
    )
    .bind(thread)
    .bind(project)
    .bind(binding_workspace_id)
    .bind(discovery_at(path))
    .execute(&mut *connection)
    .await
    .unwrap();
}

async fn table_exists(connection: &mut SqliteConnection, name: &str) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .bind(name)
            .fetch_optional(&mut *connection)
            .await
            .unwrap();
    row.is_some()
}

async fn version_of(connection: &mut SqliteConnection) -> i64 {
    sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap()
}

async fn index_exists(connection: &mut SqliteConnection, name: &str) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?1")
            .bind(name)
            .fetch_optional(&mut *connection)
            .await
            .unwrap();
    row.is_some()
}

/// 同一路径、同一 id：v12 计划沿用旧 UUID 的常见形状（真实库 27/34 行如此）。
/// 归属行拿到登记证据，绑定原样，登记表消失。
#[tokio::test]
async fn backfills_evidence_on_the_owner_row_and_drops_registrations() {
    let fixture = Fixture::open(18).await;
    let mut connection = fixture.connection().await;
    insert_workspace(
        &mut connection,
        "11111111-1111-4111-8111-111111111111",
        "/repo",
        "22222222-2222-4222-8222-222222222222",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    insert_session(
        &mut connection,
        "thread-1",
        "/repo",
        "22222222-2222-4222-8222-222222222222",
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;

    migrate_local_v19(&mut connection).await.unwrap();

    assert_eq!(version_of(&mut connection).await, 19);
    assert!(!table_exists(&mut connection, "legacy_execution_registrations").await);
    let row: (String, String, String, String) = sqlx::query_as(
        "SELECT path, project_id, identity, discovery FROM workspaces WHERE id = ?1",
    )
    .bind("22222222-2222-4222-8222-222222222222")
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(row.0, "/repo");
    assert_eq!(row.1, "11111111-1111-4111-8111-111111111111");
    assert_eq!(row.2, r#"{"device":1,"inode":1}"#);
    assert_eq!(row.3, discovery_at("/repo"));
    let binding: (String, String, String, String) = sqlx::query_as(
        "SELECT workspace_id, project_id, relative_cwd, evidence_origin FROM session_bindings",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(binding.0, "22222222-2222-4222-8222-222222222222");
    assert_eq!(binding.1, "11111111-1111-4111-8111-111111111111");
    assert_eq!(binding.2, "");
    assert_eq!(binding.3, "creation_snapshot");
    assert!(index_exists(&mut connection, "idx_bindings_project").await);
    assert!(index_exists(&mut connection, "idx_bindings_workspace").await);
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
}

/// 绑定指向的登记 id 与归属行 id 不同（v12 之后新建目录的正常形状）：
/// 绑定收敛到会话归属行，证据落到同一行。
#[tokio::test]
async fn rewires_binding_to_the_session_owner_row() {
    let fixture = Fixture::open(18).await;
    let mut connection = fixture.connection().await;
    insert_workspace(
        &mut connection,
        "11111111-1111-4111-8111-111111111111",
        "/fresh",
        "33333333-3333-4333-8333-333333333333",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    insert_session(
        &mut connection,
        "thread-1",
        "/fresh",
        "33333333-3333-4333-8333-333333333333",
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;

    migrate_local_v19(&mut connection).await.unwrap();

    assert_eq!(version_of(&mut connection).await, 19);
    let (binding_workspace, owner): (String, String) =
        sqlx::query_as("SELECT b.workspace_id, t.workspace_id FROM session_bindings b JOIN threads t ON t.id = b.thread_id")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(binding_workspace, "33333333-3333-4333-8333-333333333333");
    assert_eq!(binding_workspace, owner);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(rows, 1, "同路径只保留归属行，不再为登记新造一条");
    let evidence: (String, Option<String>) =
        sqlx::query_as("SELECT path, discovery FROM workspaces WHERE id = ?1")
            .bind("33333333-3333-4333-8333-333333333333")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(evidence.0, "/fresh");
    assert_eq!(evidence.1.unwrap(), discovery_at("/fresh"));
}

/// 7 行「有登记、没有归属行」的形状：并入归属表，保住无绑定会话的路径与项目映射。
#[tokio::test]
async fn adopts_registrations_without_an_owner_row() {
    let fixture = Fixture::open(18).await;
    let mut connection = fixture.connection().await;
    insert_workspace(
        &mut connection,
        "11111111-1111-4111-8111-111111111111",
        "/legacy-only",
        "22222222-2222-4222-8222-222222222222",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    // 只有登记、没有归属行，也没有绑定引用它。
    sqlx::query("DELETE FROM workspaces")
        .execute(&mut connection)
        .await
        .unwrap();

    migrate_local_v19(&mut connection).await.unwrap();

    let row: (String, String, String, String, String) =
        sqlx::query_as("SELECT id, machine_id, path, path_source, project_id FROM workspaces")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(row.0, "22222222-2222-4222-8222-222222222222");
    assert_eq!(row.1, fixture.machine);
    assert_eq!(row.2, "/legacy-only");
    assert_eq!(row.3, "unverified");
    assert_eq!(row.4, "11111111-1111-4111-8111-111111111111");
}

/// 同一路径的多条登记（目录对象被替换过的历史）只并入一条，不违反同机同路径唯一约束。
#[tokio::test]
async fn merges_multiple_registrations_of_one_path_into_a_single_row() {
    let fixture = Fixture::open(18).await;
    let mut connection = fixture.connection().await;
    insert_workspace(
        &mut connection,
        "11111111-1111-4111-8111-111111111111",
        "/replaced",
        "22222222-2222-4222-8222-222222222222",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    sqlx::query("DELETE FROM workspaces")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO legacy_execution_registrations(id, project_id, root, root_identity, discovery)
         VALUES ('44444444-4444-4444-8444-444444444444', '11111111-1111-4111-8111-111111111111',
                 '/replaced', '{\"device\":9,\"inode\":9}', ?1)",
    )
    .bind(discovery_at("/replaced"))
    .execute(&mut connection)
    .await
    .unwrap();

    migrate_local_v19(&mut connection).await.unwrap();

    let row: (String, String) = sqlx::query_as("SELECT id, path FROM workspaces")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(
        row.0, "22222222-2222-4222-8222-222222222222",
        "取 id 最小的一条"
    );
    assert_eq!(row.1, "/replaced");
}

/// 绑定记录的根与会话归属行的路径不一致：改写会静默改绑，拒绝升级并回滚到 18。
#[tokio::test]
async fn refuses_to_rewire_a_binding_recorded_at_a_foreign_root() {
    let fixture = Fixture::open(18).await;
    let mut connection = fixture.connection().await;
    insert_workspace(
        &mut connection,
        "11111111-1111-4111-8111-111111111111",
        "/owner",
        "22222222-2222-4222-8222-222222222222",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    insert_workspace(
        &mut connection,
        "55555555-5555-4555-8555-555555555555",
        "/elsewhere",
        "66666666-6666-4666-8666-666666666666",
        "77777777-7777-4777-8777-777777777777",
    )
    .await;
    insert_session(
        &mut connection,
        "thread-1",
        "/owner",
        "22222222-2222-4222-8222-222222222222",
        "55555555-5555-4555-8555-555555555555",
        "77777777-7777-4777-8777-777777777777",
    )
    .await;

    let error = migrate_local_v19(&mut connection).await.unwrap_err();

    assert!(error.to_string().contains("different root"), "{error}");
    assert_eq!(version_of(&mut connection).await, 18);
    assert!(table_exists(&mut connection, "legacy_execution_registrations").await);
    let binding: (String,) = sqlx::query_as("SELECT workspace_id FROM session_bindings")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(binding.0, "77777777-7777-4777-8777-777777777777");
    let columns: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM pragma_table_info('workspaces')")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert!(
        !columns.iter().any(|(name,)| name == "discovery"),
        "回滚后不留下半迁移的列"
    );
}

/// 会话的归属行不存在（`threads.workspace_id` 悬空）：没有可搬运的归属，拒绝升级。
#[tokio::test]
async fn refuses_bindings_without_a_session_workspace_owner() {
    let fixture = Fixture::open(18).await;
    let mut connection = fixture.connection().await;
    insert_workspace(
        &mut connection,
        "11111111-1111-4111-8111-111111111111",
        "/repo",
        "22222222-2222-4222-8222-222222222222",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    insert_session(
        &mut connection,
        "thread-1",
        "/repo",
        "22222222-2222-4222-8222-222222222222",
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    // 归属行与会话引用的登记在会话之后一起消失（外键关闭时写下的形状）：没有任何
    // 可搬运、可补建归属的对象。
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workspaces")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("DELETE FROM legacy_execution_registrations")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut connection)
        .await
        .unwrap();

    let error = migrate_local_v19(&mut connection).await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains("without a session workspace owner"),
        "{error}"
    );
    assert_eq!(version_of(&mut connection).await, 18);
}

/// 登记表是别的类型（同名 VIEW）：不猜它的内容，拒绝升级。
#[tokio::test]
async fn refuses_an_unrecognized_registration_shape() {
    let fixture = Fixture::open(18).await;
    let mut connection = fixture.connection().await;
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("DROP TABLE legacy_execution_registrations")
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE legacy_execution_registrations (id TEXT PRIMARY KEY)")
        .execute(&mut connection)
        .await
        .unwrap();

    let error = migrate_local_v19(&mut connection).await.unwrap_err();

    assert!(error.to_string().contains("unrecognized"), "{error}");
    assert_eq!(version_of(&mut connection).await, 18);
}

/// v12 起点也能走到 19：先补 v18 的形状，再并入登记并删表（整条链一次写打开完成）。
#[tokio::test]
async fn upgrades_a_v12_database_all_the_way_to_v19() {
    let fixture = Fixture::open(12).await;
    let mut connection = fixture.connection().await;
    insert_workspace(
        &mut connection,
        "11111111-1111-4111-8111-111111111111",
        "/repo",
        "22222222-2222-4222-8222-222222222222",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    insert_session(
        &mut connection,
        "thread-1",
        "/repo",
        "22222222-2222-4222-8222-222222222222",
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    connection.close().await.unwrap();

    let store = crate::sessions::sqlite_store::SqliteThreadStore::new(&fixture.path)
        .await
        .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(version, 19);
    let tables: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .fetch_all(&store.database.pool)
            .await
            .unwrap();
    assert!(
        !tables
            .iter()
            .any(|(name,)| name == "legacy_execution_registrations"),
        "{tables:?}"
    );
    let binding: (String, String) = sqlx::query_as(
        "SELECT b.workspace_id, t.workspace_id FROM session_bindings b JOIN threads t ON t.id = b.thread_id",
    )
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    assert_eq!(binding.0, binding.1);
    let row: (String, String) = sqlx::query_as("SELECT path, project_id FROM workspaces")
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    assert_eq!(row.0, "/repo");
    assert_eq!(row.1, "11111111-1111-4111-8111-111111111111");
    store.close().await;
}

/// 第二次打开是空操作：v19 的写打开不再触碰登记表。
#[tokio::test]
async fn reopening_a_v19_database_stays_at_v19() {
    let fixture = Fixture::open(18).await;
    let mut connection = fixture.connection().await;
    insert_workspace(
        &mut connection,
        "11111111-1111-4111-8111-111111111111",
        "/repo",
        "22222222-2222-4222-8222-222222222222",
        "22222222-2222-4222-8222-222222222222",
    )
    .await;
    connection.close().await.unwrap();

    let store = crate::sessions::sqlite_store::SqliteThreadStore::new(&fixture.path)
        .await
        .unwrap();
    store.close().await;
    let mut connection = fixture.connection().await;
    assert_eq!(version_of(&mut connection).await, 19);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(rows, 1);
}
