//! 本机单条搬运（压缩前形状 → 11）的夹具与验收：形状、证据搬迁、拒绝面与回滚。
//!
//! 夹具**手写压缩前形状（V10）的 DDL**（不引用 `canonical::V10_CREATE_*`）：这些语句是历史
//! 形状的快照，升级实现改动时它们不该跟着漂移，否则「从上一版升级」这件事就没有被验证。

use super::*;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};

/// 压缩前形状（正式基线 ≤10）：登记语义的 `workspaces`、执行环境表、machine 作用域凭证。
const V10_DDL: &[&str] = &[
    "CREATE TABLE threads (
        id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
        parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
        cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT,
        frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active')",
    "CREATE TABLE messages (
        message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        role TEXT NOT NULL, content TEXT NOT NULL,
        truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT)",
    "CREATE TABLE projects (
        id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
        UNIQUE(locator, object_identity))",
    "CREATE TABLE workspaces (
        id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
        root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
        UNIQUE(root, root_identity), UNIQUE(id, project_id))",
    "CREATE TABLE session_bindings (
        thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
        schema_version INTEGER NOT NULL,
        project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
        FOREIGN KEY(workspace_id, project_id) REFERENCES workspaces(id, project_id))",
    "CREATE TABLE session_environments (
        thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
        machine_id TEXT NOT NULL)",
    "CREATE TABLE mcp_oauth_credentials (
        principal_id TEXT NOT NULL, machine_id TEXT NOT NULL, server_key TEXT NOT NULL,
        credentials_blob TEXT NOT NULL, updated_at TEXT NOT NULL,
        PRIMARY KEY (principal_id, machine_id, server_key))",
    "CREATE INDEX idx_messages_thread_id ON messages(thread_id)",
    "CREATE INDEX idx_bindings_project ON session_bindings(project_id, thread_id)",
    "CREATE INDEX idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id)",
    "CREATE INDEX idx_threads_updated ON threads(updated_at DESC, id DESC) WHERE hidden = 0 AND message_count > 0",
    "CREATE INDEX idx_session_environments_machine ON session_environments(machine_id, thread_id)",
];

/// 旧登记的观测证据：`root` 由夹具替换，`common_dir` 缺省（路径来源判为 unverified）。
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
    async fn open(version: i64) -> Self {
        crate::sessions::machine::initialize().await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("threads.db");
        let mut connection = connect(&path).await;
        for statement in V10_DDL {
            // 语句文本来自本文件的静态清单，不含外部输入。
            sqlx::raw_sql(sqlx::AssertSqlSafe((*statement).to_owned()))
                .execute(&mut connection)
                .await
                .unwrap();
        }
        // 版本号来自本文件的静态取值（10 / 11），不含外部输入。
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
            machine: crate::sessions::machine::current().unwrap().to_owned(),
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

/// 一条旧登记：项目 + 登记行（压缩前形状里 `workspaces` 就是登记表）。
async fn insert_registration(
    connection: &mut SqliteConnection,
    project: &str,
    root: &str,
    registration: &str,
    discovery: &str,
) {
    sqlx::query("INSERT OR IGNORE INTO projects(id, locator, object_identity) VALUES (?1, ?2, ?3)")
        .bind(project)
        .bind(root)
        .bind(r#"{"device":1,"inode":1}"#)
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workspaces(id, project_id, root, root_identity, discovery)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(registration)
    .bind(project)
    .bind(root)
    .bind(r#"{"device":1,"inode":1}"#)
    .bind(discovery)
    .execute(&mut *connection)
    .await
    .unwrap();
}

/// 一条会话行 + 一条绑定行 + 一条环境行（`registration` 是绑定自己记录的执行身份）。
async fn insert_session(
    connection: &mut SqliteConnection,
    thread: &str,
    cwd: &str,
    project: &str,
    registration: &str,
    machine: &str,
) {
    sqlx::query(
        "INSERT INTO threads(id, cwd, created_at, updated_at) VALUES (?1, ?2, 'now', 'now')",
    )
    .bind(thread)
    .bind(cwd)
    .execute(&mut *connection)
    .await
    .unwrap();
    sqlx::query("INSERT INTO session_bindings VALUES (?1, 1, ?2, ?3, '')")
        .bind(thread)
        .bind(project)
        .bind(registration)
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO session_environments(thread_id, machine_id) VALUES (?1, ?2)")
        .bind(thread)
        .bind(machine)
        .execute(&mut *connection)
        .await
        .unwrap();
}

async fn version_of(connection: &mut SqliteConnection) -> i64 {
    sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await
        .unwrap()
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

async fn index_exists(connection: &mut SqliteConnection, name: &str) -> bool {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?1")
            .bind(name)
            .fetch_optional(&mut *connection)
            .await
            .unwrap();
    row.is_some()
}

/// 迁移终点必须是 canonical 形状：逐表逐列（与远端判定同一份声明）且带齐 canonical 索引集。
///
/// 形状判定只看表/列；索引是派生对象，不在判定里，由这里显式断言「建齐」——搬运动过
/// `threads` / `session_bindings`，`DROP TABLE` 会把旧索引一并带走。
async fn assert_current_shape(connection: &mut SqliteConnection) {
    let tables = crate::sessions::schema_shape::read_local_columns(
        connection,
        crate::sessions::schema_shape::CURRENT_COLUMNS_SQL,
    )
    .await
    .unwrap();
    crate::sessions::schema_shape::check_current_shape(&tables).unwrap();
    for statement in crate::sessions::canonical::CREATE_INDEXES {
        let name = canonical_index_name(statement);
        assert!(index_exists(connection, &name).await, "{name}");
    }
}

/// 从 `CREATE INDEX IF NOT EXISTS <name> ON ...` 里取出索引名（canonical 的语句形态固定）。
fn canonical_index_name(statement: &str) -> String {
    statement
        .split_whitespace()
        .skip_while(|token| *token != "EXISTS")
        .nth(1)
        .expect("canonical index statement must name the index")
        .to_owned()
}

async fn assert_no_foreign_key_violation(connection: &mut SqliteConnection) {
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut *connection)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");
}

/// 同路径、同 id：归属行沿用旧登记 UUID，证据三列随行搬过去，凭证与形状退役。
#[tokio::test]
async fn upgrades_the_registration_shape_preserving_execution_evidence() {
    let fixture = Fixture::open(11).await;
    let mut connection = fixture.connection().await;
    let project = "11111111-1111-4111-8111-111111111111";
    let registration = "22222222-2222-4222-8222-222222222222";
    insert_registration(
        &mut connection,
        project,
        "/repo",
        registration,
        &discovery_at("/repo"),
    )
    .await;
    insert_session(
        &mut connection,
        "thread-1",
        "/repo",
        project,
        registration,
        &fixture.machine,
    )
    .await;
    sqlx::query(
        "INSERT INTO mcp_oauth_credentials VALUES ('local', ?1, 'server', 'old token', 'now')",
    )
    .bind(&fixture.machine)
    .execute(&mut connection)
    .await
    .unwrap();

    migrate_local_v11(&mut connection).await.unwrap();

    assert_eq!(version_of(&mut connection).await, 11);
    assert_current_shape(&mut connection).await;
    assert!(!table_exists(&mut connection, "session_environments").await);
    assert!(!table_exists(&mut connection, "legacy_execution_registrations").await);
    assert!(index_exists(&mut connection, "idx_bindings_project").await);
    assert!(index_exists(&mut connection, "idx_bindings_workspace").await);
    assert!(index_exists(&mut connection, "idx_threads_workspace_archived").await);
    let workspace: (String, String, String, String, String, String) = sqlx::query_as(
        "SELECT id, machine_id, path, path_source, project_id, identity FROM workspaces",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(workspace.0, registration, "归属行沿用旧登记 UUID");
    assert_eq!(workspace.1, fixture.machine);
    assert_eq!(workspace.2, "/repo");
    assert_eq!(workspace.3, "unverified");
    assert_eq!(workspace.4, project);
    assert_eq!(workspace.5, r#"{"device":1,"inode":1}"#);
    let (discovery,): (String,) = sqlx::query_as("SELECT discovery FROM workspaces WHERE id = ?1")
        .bind(registration)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(discovery, discovery_at("/repo"));

    let thread: (String, i64, String) =
        sqlx::query_as("SELECT workspace_id, archived, cwd FROM threads WHERE id = 'thread-1'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(thread.0, registration);
    assert_eq!(thread.1, 0);
    assert_eq!(thread.2, "/repo");

    let binding: (String, String, String, String, String) = sqlx::query_as(
        "SELECT workspace_id, project_id, relative_cwd, discovery_snapshot, evidence_origin
         FROM session_bindings WHERE thread_id = 'thread-1'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(binding.0, registration, "绑定收敛到会话归属行");
    assert_eq!(binding.1, project);
    assert_eq!(binding.2, "");
    assert_eq!(binding.3, discovery_at("/repo"), "证据取旧登记的最后观测");
    assert_eq!(binding.4, "legacy_last_observation");

    let credentials: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mcp_oauth_credentials")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(credentials, 0, "machine 作用域凭证无法证明 Workspace 归属");
    assert_no_foreign_key_violation(&mut connection).await;
}

/// 同一路径的两条登记（目录对象被替换过的历史）并成一条归属行，绑定各留各的观测证据。
#[tokio::test]
async fn merges_registrations_of_one_path_and_keeps_per_binding_snapshots() {
    let fixture = Fixture::open(11).await;
    let mut connection = fixture.connection().await;
    let project = "11111111-1111-4111-8111-111111111111";
    let first = "22222222-2222-4222-8222-222222222222";
    let second = "44444444-4444-4444-8444-444444444444";
    let second_discovery = discovery_at("/replaced").replace("\"inode\":1", "\"inode\":2");
    insert_registration(
        &mut connection,
        project,
        "/replaced",
        first,
        &discovery_at("/replaced"),
    )
    .await;
    sqlx::query(
        "INSERT INTO workspaces(id, project_id, root, root_identity, discovery)
         VALUES (?1, ?2, '/replaced', '{\"device\":1,\"inode\":2}', ?3)",
    )
    .bind(second)
    .bind(project)
    .bind(&second_discovery)
    .execute(&mut connection)
    .await
    .unwrap();
    insert_session(
        &mut connection,
        "thread-1",
        "/replaced",
        project,
        first,
        &fixture.machine,
    )
    .await;
    insert_session(
        &mut connection,
        "thread-2",
        "/replaced",
        project,
        second,
        &fixture.machine,
    )
    .await;

    migrate_local_v11(&mut connection).await.unwrap();

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspaces")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(rows, 1, "同机同路径只留一行");
    let owners: Vec<(String,)> = sqlx::query_as("SELECT workspace_id FROM threads ORDER BY id")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert_eq!(owners.len(), 2);
    assert_eq!(owners[0].0, owners[1].0, "两个会话归到同一条归属行");
    let snapshots: Vec<(String,)> =
        sqlx::query_as("SELECT discovery_snapshot FROM session_bindings ORDER BY thread_id")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert_eq!(snapshots[0].0, discovery_at("/replaced"));
    assert_eq!(snapshots[1].0, second_discovery, "证据按绑定各自保留");
    assert_no_foreign_key_violation(&mut connection).await;
}

/// 没有会话引用的登记行不凭空消失：保留为归属行（无会话时退到本机第一台机器）。
#[tokio::test]
async fn keeps_a_registration_that_no_session_references() {
    let fixture = Fixture::open(11).await;
    let mut connection = fixture.connection().await;
    let project = "11111111-1111-4111-8111-111111111111";
    let registration = "22222222-2222-4222-8222-222222222222";
    insert_registration(
        &mut connection,
        project,
        "/legacy-only",
        registration,
        &discovery_at("/legacy-only"),
    )
    .await;

    migrate_local_v11(&mut connection).await.unwrap();

    let row: (String, String, String, String) =
        sqlx::query_as("SELECT id, machine_id, path, path_source FROM workspaces")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(row.0, registration);
    assert_eq!(row.1, fixture.machine);
    assert_eq!(row.2, "/legacy-only");
    assert_eq!(row.3, "unverified");
    let machines: Vec<(String,)> = sqlx::query_as("SELECT id FROM machines")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert_eq!(machines.len(), 1, "没有旧机器时只登记当前机器");
    assert_eq!(machines[0].0, fixture.machine);
}

/// 绑定引用的登记行不存在（外键关闭时写下的形状）：没有可收敛的归属证据，
/// 拒绝升级并回滚，库保持原版本。
#[tokio::test]
async fn refuses_a_binding_referencing_a_missing_registration() {
    let fixture = Fixture::open(11).await;
    let mut connection = fixture.connection().await;
    let project = "11111111-1111-4111-8111-111111111111";
    let registration = "22222222-2222-4222-8222-222222222222";
    insert_registration(
        &mut connection,
        project,
        "/repo",
        registration,
        &discovery_at("/repo"),
    )
    .await;
    insert_session(
        &mut connection,
        "thread-1",
        "/repo",
        project,
        registration,
        &fixture.machine,
    )
    .await;
    // 登记行消失而绑定还引用它：绑定记录的执行身份在本次搬运里找不到对应证据。
    sqlx::raw_sql("PRAGMA foreign_keys = OFF; DELETE FROM workspaces;")
        .execute(&mut connection)
        .await
        .unwrap();

    let error = migrate_local_v11(&mut connection).await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains("missing legacy execution registration"),
        "{error}"
    );
    assert_eq!(version_of(&mut connection).await, 11);
    let binding: (String,) =
        sqlx::query_as("SELECT workspace_id FROM session_bindings WHERE thread_id = 'thread-1'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(binding.0, registration, "回滚后绑定未被改写");
    let columns: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM pragma_table_info('workspaces')")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    assert!(
        !columns.iter().any(|(name,)| name == "machine_id"),
        "回滚后不留下半迁移的归属表"
    );
}

/// 绑定没有可搬运的会话归属（`threads` 行缺失）：拒绝升级，库保持原版本。
#[tokio::test]
async fn refuses_a_binding_without_a_session_workspace_owner() {
    let fixture = Fixture::open(11).await;
    let mut connection = fixture.connection().await;
    let project = "11111111-1111-4111-8111-111111111111";
    let registration = "22222222-2222-4222-8222-222222222222";
    insert_registration(
        &mut connection,
        project,
        "/repo",
        registration,
        &discovery_at("/repo"),
    )
    .await;
    insert_session(
        &mut connection,
        "thread-1",
        "/repo",
        project,
        registration,
        &fixture.machine,
    )
    .await;
    // 会话行消失而绑定行留下（外键关闭时写下的形状）：没有任何可搬运的归属。
    sqlx::raw_sql("PRAGMA foreign_keys = OFF; DELETE FROM threads;")
        .execute(&mut connection)
        .await
        .unwrap();

    let error = migrate_local_v11(&mut connection).await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains("legacy binding has no session workspace owner"),
        "{error}"
    );
    assert_eq!(version_of(&mut connection).await, 11);
    assert!(table_exists(&mut connection, "session_environments").await);
}

/// 正式发布的 ≤10 库（这里取 10）经写打开补齐并搬运到当前形状，第二次打开是空操作。
#[tokio::test]
async fn released_schema_10_database_upgrades_through_the_store_once() {
    let fixture = Fixture::open(10).await;
    let mut connection = fixture.connection().await;
    let project = "11111111-1111-4111-8111-111111111111";
    let registration = "22222222-2222-4222-8222-222222222222";
    insert_registration(
        &mut connection,
        project,
        "/repo",
        registration,
        &discovery_at("/repo"),
    )
    .await;
    insert_session(
        &mut connection,
        "thread-1",
        "/repo",
        project,
        registration,
        &fixture.machine,
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
    assert_eq!(version, 11);
    let mut connection = store.database.pool.acquire().await.unwrap();
    assert_current_shape(&mut connection).await;
    drop(connection);
    let (cookie,): (i64,) = sqlx::query_as("PRAGMA schema_version")
        .fetch_one(&store.database.pool)
        .await
        .unwrap();
    let row: (String, String, String) = sqlx::query_as(
        "SELECT t.workspace_id, w.path, b.evidence_origin FROM threads t
         JOIN workspaces w ON w.id = t.workspace_id
         JOIN session_bindings b ON b.thread_id = t.id WHERE t.id = 'thread-1'",
    )
    .fetch_one(&store.database.pool)
    .await
    .unwrap();
    assert_eq!(row.0, registration);
    assert_eq!(row.1, "/repo");
    assert_eq!(row.2, "legacy_last_observation");
    store.close().await;

    let reopened = crate::sessions::sqlite_store::SqliteThreadStore::new(&fixture.path)
        .await
        .unwrap();
    let (reopened_cookie,): (i64,) = sqlx::query_as("PRAGMA schema_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(reopened_cookie, cookie, "DDL 只跑一次");
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&reopened.database.pool)
        .await
        .unwrap();
    assert_eq!(version, 11);
    reopened.close().await;
}
