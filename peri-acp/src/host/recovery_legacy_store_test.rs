//! 旧库（schema 13）的进入语义（设计 §9「旧库升级」一行、§3「只读 Store 不能放行执行」）。
//!
//! 阶段 2 已在 store 层验证「只读打开旧库读作 `idle`」；这里补 wire 层的两条：
//!
//! 1. 旧库按生产意图（读写）进入：只补两列，旧 Active 不触发恢复，历史与 MQ 内容不被改写；
//! 2. 只读 Store 进入：返回可浏览的视图（历史/占用），执行被阻断、零写入——恢复不放行执行，也不悄悄升级旧库。

use super::*;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session::MqFilter;
use peri_acp_types::session_resources::AccessMode;
use peri_acp_types::session_store::SessionStoreDeployment;
use peri_acp_types::store::PersistedPayload;
use peri_resources::sessions::open_store_and_facade_for_tests;
use sqlx::Connection as _;

const V13: i64 = 13;
const LEGACY_MESSAGE: &str = "interrupted before upgrade";

/// 生产路径建一条会话（含 frozen 上下文）与一条历史，再写一条待发 MQ 行，最后退回
/// schema 13 形状。
///
/// 关库用 `close()` 而非 `Drop`：只读打开要等在 WAL 收尾之后，否则会撞上 mmap 独占锁。
async fn seed_schema_v13(db_path: &std::path::Path, cwd: &str) -> String {
    let (bridge, facade) = open_store_and_facade_for_tests(db_path).await.unwrap();
    let mut seed = RecoveryHarness::assemble(
        tempfile::tempdir().unwrap(),
        db_path.to_path_buf(),
        cwd.to_owned(),
        Arc::new(facade),
        false,
    )
    .await;
    seed.create_session().await;
    let thread_id = seed.thread_id.clone();
    seed.cfg
        .session_resources
        .append_history(
            &thread_id,
            &[PersistedPayload::Message(BaseMessage::human(
                LEGACY_MESSAGE,
            ))],
        )
        .await
        .unwrap();
    seed.queue(&thread_id)
        .enqueue(peri_acp_types::session::QueuedMessage::new(
            peri_acp_types::session::MessageKind::Info,
            peri_acp_types::session::MessageSource::SystemInjected,
            BaseMessage::human("queued before upgrade"),
        ))
        .await
        .unwrap();
    let cfg = Arc::clone(&seed.cfg);
    drop(seed);
    drop(cfg);
    bridge.close().await;
    downgrade_to_v13(db_path).await;
    thread_id
}

async fn downgrade_to_v13(db_path: &std::path::Path) {
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(db_path)
            // 重命名期间不强制外键；各表重建完成后再由打开路径的检查兜底。
            .foreign_keys(false),
    )
    .await
    .unwrap();
    // v15 重建过 projects/workspaces/session_bindings 并退役了两张 inherited 表，只删
    // threads 两列会得到任何已知代际都不匹配的混合形状。这里按 v14 冻结声明补回三表
    // 形状与两张空派生表，得到真正的 v13 库；行数据原样保留。
    sqlx::raw_sql(
        "CREATE TABLE projects_next (
             id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
             UNIQUE(locator, object_identity));
         INSERT INTO projects_next (id, locator, object_identity)
             SELECT id, locator, '{\"device\":1,\"inode\":2}' FROM projects;
         DROP TABLE projects;
         ALTER TABLE projects_next RENAME TO projects;

         CREATE TABLE workspaces_next (
             id TEXT PRIMARY KEY, machine_id TEXT NOT NULL REFERENCES machines(id),
             path TEXT NOT NULL,
             path_source TEXT NOT NULL CHECK(path_source IN ('discovered', 'derived_legacy', 'unverified')),
             project_id TEXT REFERENCES projects(id),
             identity TEXT, discovery TEXT,
             UNIQUE(machine_id, path));
         INSERT INTO workspaces_next
             (id, machine_id, path, path_source, project_id, discovery)
             SELECT id, machine_id, path, path_source, project_id, discovery
             FROM workspaces;
         DROP TABLE workspaces;
         ALTER TABLE workspaces_next RENAME TO workspaces;

         CREATE TABLE session_bindings_next (
             thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
             schema_version INTEGER NOT NULL,
             project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
             discovery_snapshot TEXT, evidence_origin TEXT NOT NULL);
         INSERT INTO session_bindings_next
             (thread_id, schema_version, project_id, workspace_id, relative_cwd,
              discovery_snapshot, evidence_origin)
             SELECT thread_id, schema_version, project_id, workspace_id, relative_cwd,
                    discovery_snapshot, 'creation_snapshot'
             FROM session_bindings;
         DROP TABLE session_bindings;
         ALTER TABLE session_bindings_next RENAME TO session_bindings;

         CREATE TABLE inherited_snapshot_index (
             snapshot_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, payload_bytes INTEGER NOT NULL,
             entry_count INTEGER NOT NULL, prepared_at TEXT NOT NULL);
         CREATE TABLE inherited_payload_index (
             snapshot_id TEXT NOT NULL, ordinal INTEGER NOT NULL, entry_id TEXT NOT NULL,
             raw_start INTEGER NOT NULL, raw_len INTEGER NOT NULL, logical_len INTEGER,
             PRIMARY KEY(snapshot_id, ordinal), UNIQUE(snapshot_id, entry_id));

         -- 旧库遗留的 Active 状态：v13 没有工作意图列，Active 不是自动续跑的依据。
         UPDATE threads SET agent_status = 'active';
         ALTER TABLE threads DROP COLUMN work_state;
         ALTER TABLE threads DROP COLUMN context_usage_tokens;
         PRAGMA user_version = 13;",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
}

/// 当前代际以新建库为准：测试不重复写版本号常量。
async fn current_schema_version() -> i64 {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    let store = peri_resources::sessions::SqliteThreadStore::new(&path)
        .await
        .unwrap();
    store.close().await;
    read_shape(&path).await.0
}

async fn read_shape(db_path: &std::path::Path) -> (i64, Vec<String>) {
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(db_path),
    )
    .await
    .unwrap();
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let columns: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info('threads')")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    (version, columns.into_iter().map(|(name,)| name).collect())
}

/// 经真实 `ServerLoop` 发 `initialize` + `session/load`。
fn load_through_server_loop(harness: &RecoveryHarness, thread_id: &str, cwd: &str) {
    super::wire_tests::push_request(
        harness,
        1,
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {"_meta": {}}}),
    );
    super::wire_tests::push_request(
        harness,
        2,
        "session/load",
        json!({"sessionId": thread_id, "cwd": cwd}),
    );
}

fn load_result(harness: &RecoveryHarness) -> bool {
    harness
        .recorded
        .responses()
        .iter()
        .find(|(id, _)| *id == RequestId::Number(2))
        .expect("session/load 必须有响应")
        .1
        .is_ok()
}

/// 1. 读写进入 schema 13 库：load 成功、零恢复；只补两列，历史与 MQ 内容原样。
#[tokio::test]
async fn schema_v13_upgrade_on_entry_recovers_nothing_and_keeps_history_and_queue() {
    let workspace = tempfile::tempdir().unwrap();
    let db_path = workspace.path().join("threads.db");
    let cwd = workspace.path().to_string_lossy().into_owned();
    let thread_id = seed_schema_v13(&db_path, &cwd).await;
    let (version, columns) = read_shape(&db_path).await;
    assert_eq!(version, V13);
    assert!(!columns.iter().any(|name| name == "work_state"));

    let harness = RecoveryHarness::durable(workspace.path()).await;
    load_through_server_loop(&harness, &thread_id, &cwd);
    super::wire_tests::run_server_loop(&harness).await;

    assert!(load_result(&harness), "旧库读写进入必须成功");
    assert!(
        harness.recovery_rows(&thread_id).await.is_empty(),
        "旧 Active 不得触发自动恢复"
    );
    assert!(!harness._mock.matched(), "旧库不得启动 turn");

    let (version, columns) = read_shape(&db_path).await;
    assert_eq!(
        version,
        current_schema_version().await,
        "读写打开按当前代际升级"
    );
    assert_eq!(
        columns.iter().filter(|name| *name == "work_state").count(),
        1,
        "只补一列工作意图"
    );
    let (state, usage): (String, Option<i64>) = {
        let mut connection = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(&db_path),
        )
        .await
        .unwrap();
        let row: (String, Option<i64>) =
            sqlx::query_as("SELECT work_state, context_usage_tokens FROM threads WHERE id = ?1")
                .bind(&thread_id)
                .fetch_one(&mut connection)
                .await
                .unwrap();
        connection.close().await.unwrap();
        row
    };
    assert_eq!(state, "idle", "旧行升级后不得自动续跑");
    assert_eq!(usage, None, "未知占用不是零占用");

    let queued = harness
        .queue(&thread_id)
        .query(MqFilter::default())
        .await
        .unwrap();
    assert_eq!(queued.len(), 1, "v13 已有的 MQ 行必须保留");
    let (count, content) = {
        let mut connection = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(&db_path),
        )
        .await
        .unwrap();
        let row: (i64, String) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(MAX(content), '') FROM messages WHERE thread_id = ?1",
        )
        .bind(&thread_id)
        .fetch_one(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        row
    };
    assert_eq!(count, 1, "历史不得被改写");
    assert!(
        content.contains(LEGACY_MESSAGE),
        "历史内容原样保留：{content}"
    );
}

/// 2. 只读进入 schema 13 库：历史可浏览、执行被阻断、库文件零写入（不升级、不改形状）。
#[tokio::test]
async fn schema_v13_read_only_entry_serves_history_with_execution_block() {
    let workspace = tempfile::tempdir().unwrap();
    let db_path = workspace.path().join("threads.db");
    let cwd = workspace.path().to_string_lossy().into_owned();
    let thread_id = seed_schema_v13(&db_path, &cwd).await;
    let before_shape = read_shape(&db_path).await;
    let before_bytes = std::fs::read(&db_path).unwrap();

    let deployment =
        SessionStoreDeployment::local_path(db_path.clone()).with_access(AccessMode::ReadOnly);
    let (facade, _shutdown) = peri_agent::resources::open_session_resources_deployment(&deployment)
        .await
        .unwrap();
    let harness = RecoveryHarness::assemble(
        tempfile::tempdir().unwrap(),
        db_path.clone(),
        cwd.clone(),
        facade,
        false,
    )
    .await;
    load_through_server_loop(&harness, &thread_id, &cwd);
    super::wire_tests::run_server_loop(&harness).await;

    let responses = harness.recorded.responses();
    let (_, result) = responses
        .iter()
        .find(|(id, _)| *id == RequestId::Number(2))
        .expect("session/load 必须有响应");
    let response = result
        .as_ref()
        .expect("只读 Store 必须返回可浏览的视图，而不是失败整次 load");
    let recovery = &response["_meta"]["peri.recovery"];
    assert_eq!(recovery["status"], "failed", "{response}");
    assert_eq!(recovery["error"]["kind"], "read_only", "{response}");
    assert_eq!(recovery["error"]["executionBlocked"], true, "{response}");
    assert_eq!(recovery["executionBlocked"], true, "{response}");
    let notifications = harness.recorded.notifications();
    assert!(
        notifications.iter().any(|(method, params)| {
            method == "session/update" && params.to_string().contains(LEGACY_MESSAGE)
        }),
        "只读视图必须先回放历史：{notifications:?}"
    );
    assert!(!harness._mock.matched(), "只读进入不得启动 turn");
    assert_eq!(
        read_shape(&db_path).await,
        before_shape,
        "只读进入不得升级或改写旧库形状"
    );
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        before_bytes,
        "只读进入不得写入库文件"
    );

    // 视图态仍拒绝执行：prompt 在模型调用前返回 -32017。
    super::wire_tests::push_request(
        &harness,
        3,
        "session/prompt",
        json!({
            "sessionId": thread_id,
            "prompt": [{"type": "text", "text": "continue"}],
        }),
    );
    super::wire_tests::run_server_loop(&harness).await;
    wait_for("只读视图的 prompt 被阻断", || async {
        harness.recorded.error_for(RequestId::Number(3)).is_some()
    })
    .await;
    let error = harness
        .recorded
        .error_for(RequestId::Number(3))
        .expect("prompt 必须有错误响应");
    assert_eq!(error.code, -32017, "{error:?}");
    assert_eq!(
        error.data.as_ref().unwrap()["reason"],
        "recovery_execution_blocked"
    );
    assert_eq!(error.data.as_ref().unwrap()["kind"], "read_only");
    assert!(!harness._mock.matched(), "阻断必须在模型调用之前");

    // 重复加载同一只读会话：复用已发布的视图态，不得第二次报错。
    super::wire_tests::push_request(
        &harness,
        4,
        "session/load",
        json!({"sessionId": thread_id.clone(), "cwd": cwd.clone()}),
    );
    super::wire_tests::run_server_loop(&harness).await;
    let responses = harness.recorded.responses();
    let response = responses
        .iter()
        .find(|(id, _)| *id == RequestId::Number(4))
        .expect("第二次 load 必须有响应")
        .1
        .as_ref()
        .expect("第二次只读 load 必须返回视图，而不是失败");
    assert_eq!(
        response["_meta"]["peri.recovery"]["error"]["kind"],
        "read_only"
    );
}
