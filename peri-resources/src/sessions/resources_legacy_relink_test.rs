//! 迁移后无绑定历史会话的归属重建（上游缺陷报告 `session/load` 一律 `-32010` 的复现与修复）。
//!
//! 现场：迁移建出的归属行在「没有可引用的旧登记、也没有旧绑定」时只有保存 cwd 这一个
//! 归组事实（`path_source='unverified'`，证据三列全空）。这些会话没有绑定，读路径只能
//! 给出不带执行证据的展示投影，接纳因此失败——报告里的文案是
//! `session binding version or data is unsupported`，既不是真因也不可操作。
//!
//! 修复后的行为：本机按保存 cwd 的**当前观测证据**重建归属（原归属行从未被观测过时
//! 收敛过去；行上已有证据则如实失败，不静默改绑），随后接纳在同一个事务里写下绑定。

use super::*;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};

/// 压缩前形状（正式基线 ≤10）。手写快照，不引用 canonical 的 V10 清单。
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
];

async fn connect(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap()
}

async fn insert_registration(
    connection: &mut SqliteConnection,
    project: &str,
    root: &str,
    registration: &str,
    root_identity: &str,
    discovery: &str,
) {
    sqlx::query("INSERT OR IGNORE INTO projects(id, locator, object_identity) VALUES (?1, ?2, ?3)")
        .bind(project)
        .bind(root)
        .bind(root_identity)
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
    .bind(root_identity)
    .bind(discovery)
    .execute(&mut *connection)
    .await
    .unwrap();
}

async fn insert_thread(
    connection: &mut SqliteConnection,
    thread: &str,
    cwd: &str,
    machine: &str,
    message: &str,
) {
    sqlx::query(
        "INSERT INTO threads(id, cwd, created_at, updated_at)
         VALUES (?1, ?2, '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')",
    )
    .bind(thread)
    .bind(cwd)
    .execute(&mut *connection)
    .await
    .unwrap();
    // 正文列是版本化 payload 的 JSON；行 id 必须与 payload 自身的 id 一致（canonical 不变量）。
    let entry = payload(message);
    sqlx::query(
        "INSERT INTO messages(message_id, thread_id, role, content) VALUES (?1, ?2, 'human', ?3)",
    )
    .bind(entry.id().as_uuid().to_string())
    .bind(thread)
    .bind(peri_acp_types::store::serialize_persisted_payload(&entry).unwrap())
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

/// 迁移现场：真实仓库 + 子目录，一条带绑定的会话、两条无绑定的历史会话。
struct Migrated {
    facade: Arc<SessionResourcesImpl>,
    _repo: tempfile::TempDir,
    _db: tempfile::TempDir,
}

impl Migrated {
    async fn build() -> Self {
        let repo = repository();
        let repo_root = std::fs::canonicalize(repo.path()).unwrap();
        let sub = repo_root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let scratch_dir = tempfile::tempdir().unwrap();
        let scratch = SessionResourcesImpl::open(scratch_dir.path().join("threads.db"))
            .await
            .unwrap();
        let resolved = scratch.resolve_workspace(repo.path()).await.unwrap();
        let observed: (String, String, String) =
            sqlx::query_as("SELECT project_id, identity, discovery FROM workspaces WHERE id = ?1")
                .bind(resolved.workspace_id.to_string())
                .fetch_one(scratch.local_pool())
                .await
                .unwrap();
        drop(scratch);
        let (project_id, root_identity, discovery) = observed;

        let db = tempfile::tempdir().unwrap();
        let path = db.path().join("threads.db");
        let mut connection = connect(&path).await;
        for statement in V10_DDL {
            sqlx::raw_sql(sqlx::AssertSqlSafe((*statement).to_owned()))
                .execute(&mut connection)
                .await
                .unwrap();
        }
        let machine = crate::sessions::machine::initialize().await.unwrap();
        let root_text = repo_root.to_str().unwrap();
        insert_registration(
            &mut connection,
            &project_id,
            root_text,
            "22222222-2222-4222-8222-222222222222",
            &root_identity,
            &discovery,
        )
        .await;
        insert_thread(
            &mut connection,
            "bound-root",
            root_text,
            machine,
            "已绑定的历史",
        )
        .await;
        insert_thread(
            &mut connection,
            "unbound-root",
            root_text,
            machine,
            "无绑定历史（仓库根）",
        )
        .await;
        insert_thread(
            &mut connection,
            "unbound-sub",
            sub.to_str().unwrap(),
            machine,
            "无绑定历史（子目录）",
        )
        .await;
        insert_thread(
            &mut connection,
            "unbound-gone",
            db.path().join("gone").to_str().unwrap(),
            machine,
            "无绑定历史（目录已不在）",
        )
        .await;
        sqlx::query(
            "INSERT INTO session_bindings VALUES ('bound-root', 1, ?1, '22222222-2222-4222-8222-222222222222', '')",
        )
        .bind(&project_id)
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::raw_sql("PRAGMA user_version = 10")
            .execute(&mut connection)
            .await
            .unwrap();
        connection.close().await.unwrap();

        Self {
            facade: Arc::new(SessionResourcesImpl::open(&path).await.unwrap()),
            _repo: repo,
            _db: db,
        }
    }

    async fn workspace_of(&self, id: &str) -> String {
        let row: (String,) = sqlx::query_as("SELECT workspace_id FROM threads WHERE id = ?1")
            .bind(id)
            .fetch_one(self.facade.local_pool())
            .await
            .unwrap();
        row.0
    }

    async fn recorded_evidence(&self, workspace_id: &str) -> (Option<String>, Option<String>) {
        sqlx::query_as("SELECT project_id, discovery FROM workspaces WHERE id = ?1")
            .bind(workspace_id)
            .fetch_one(self.facade.local_pool())
            .await
            .unwrap()
    }

    async fn binding_rows(&self, id: &str) -> Vec<(String, String, String, String)> {
        sqlx::query_as(
            "SELECT project_id, workspace_id, relative_cwd, evidence_origin
             FROM session_bindings WHERE thread_id = ?1",
        )
        .bind(id)
        .fetch_all(self.facade.local_pool())
        .await
        .unwrap()
    }

    async fn count(&self, sql: &'static str) -> i64 {
        let row: (i64,) = sqlx::query_as(sql)
            .fetch_one(self.facade.local_pool())
            .await
            .unwrap();
        row.0
    }
}

/// 上游缺陷的回归：迁移后无绑定的子目录历史会话能按当前观测证据重新接纳并建立绑定。
#[tokio::test]
async fn migrated_unbound_history_is_relinked_from_current_evidence() {
    let store = Migrated::build().await;
    let facade = &store.facade;
    let id = "unbound-sub".to_owned();

    // 迁移现场：只有带绑定那条会话有绑定；子目录会话的归属行没有任何执行证据。
    let mut bindings: Vec<(String, String)> = sqlx::query_as(
        "SELECT thread_id, evidence_origin FROM session_bindings ORDER BY thread_id",
    )
    .fetch_all(facade.local_pool())
    .await
    .unwrap();
    bindings.sort();
    assert_eq!(
        bindings,
        vec![(
            "bound-root".to_owned(),
            "legacy_last_observation".to_owned()
        )]
    );
    let owner = store.workspace_of(&id).await;
    assert_eq!(
        store.recorded_evidence(&owner).await,
        (None, None),
        "迁移不伪造过去的执行证据"
    );
    assert_eq!(store.count("SELECT COUNT(*) FROM threads").await, 4);

    // 缺陷现场：直接接纳只能拿到展示投影（没有观测快照、项目 id 由会话 id 派生），
    // 拒绝原因必须是「本机核实不了历史归属」，而不是笼统的绑定数据不支持。
    let meta = facade.load_session_meta(&id).await.unwrap();
    let projection = facade
        .validate_bound_workspace(&id, BindingRecheck::Recorded)
        .await
        .unwrap();
    assert!(projection.discovery_snapshot.is_none());
    let refused = facade
        .adopt_legacy_session(
            &id,
            &meta.cwd,
            &projection,
            &FrozenSnapshotBytes::new("legacy".to_owned()),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error_kind(&refused),
            SessionResourceErrorKind::Workspace(WorkspaceError::UnverifiedLegacyOwner)
        ),
        "展示投影不是执行证据：{refused}"
    );

    // 修复入口：按当前证据重建归属，幂等；旧归属行保留（不静默移动旧 Workspace）。
    let relinked = facade.relink_unbound_session(&id).await.unwrap();
    assert!(
        relinked.discovery_snapshot.is_some(),
        "重建后的归属必须带本次观测证据"
    );
    assert_eq!(relinked.relative_cwd, std::path::PathBuf::from("sub"));
    assert_eq!(
        store.workspace_of(&id).await,
        relinked.workspace_id.to_string()
    );
    assert_ne!(relinked.workspace_id.to_string(), owner);
    assert_eq!(
        store
            .count("SELECT COUNT(*) FROM workspaces WHERE project_id IS NULL")
            .await,
        2
    );
    let again = facade.relink_unbound_session(&id).await.unwrap();
    assert_eq!(again.workspace_id, relinked.workspace_id);
    assert_eq!(again.discovery_snapshot, relinked.discovery_snapshot);

    // 复核与接纳：绑定与 frozen 一次写入，证据就是归属行的观测快照。
    let validated = facade
        .validate_bound_workspace(&id, BindingRecheck::Recorded)
        .await
        .unwrap();
    assert_eq!(validated.discovery_snapshot, relinked.discovery_snapshot);
    facade
        .adopt_legacy_session(
            &id,
            &meta.cwd,
            &relinked,
            &FrozenSnapshotBytes::new(r#"{"v":1}"#.to_owned()),
        )
        .await
        .unwrap();
    let rows = store.binding_rows(&id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, relinked.project_id.to_string());
    assert_eq!(rows[0].1, relinked.workspace_id.to_string());
    assert_eq!(rows[0].2, "sub");
    assert_eq!(rows[0].3, "creation_snapshot");
    let snapshot = facade.load_session_snapshot(&id).await.unwrap();
    assert!(matches!(snapshot.binding, BindingState::Bound(_)));
    assert_eq!(snapshot.payloads.len(), 1, "历史正文保持可读");
    // 再跑一次修复入口只复核既有绑定，不再改写。
    let settled = facade.relink_unbound_session(&id).await.unwrap();
    assert_eq!(settled.workspace_id, relinked.workspace_id);
    assert_eq!(store.binding_rows(&id).await.len(), 1);
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(facade.local_pool())
        .await
        .unwrap();
    assert!(violations.is_empty(), "外键检查必须为空：{violations:?}");
}

/// 本机核实不了的历史会话：可操作地失败（提示新建会话），历史仍可读，不写任何绑定。
#[tokio::test]
async fn unverifiable_history_fails_with_actionable_guidance() {
    let store = Migrated::build().await;
    let facade = &store.facade;
    let id = "unbound-gone".to_owned();

    let error = facade.relink_unbound_session(&id).await.unwrap_err();
    assert!(
        matches!(
            error_kind(&error),
            SessionResourceErrorKind::Workspace(WorkspaceError::UnverifiedLegacyOwner)
        ),
        "{error}"
    );
    assert!(
        error.to_string().contains("start a new session"),
        "文案必须给出可完成的下一步：{error}"
    );
    assert!(store.binding_rows(&id).await.is_empty());
    assert_eq!(
        store
            .recorded_evidence(&store.workspace_of(&id).await)
            .await,
        (None, None),
        "无法核实时不改写归属行的证据"
    );
    let snapshot = facade.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.payloads.len(), 1, "历史正文保持可读");
}

/// 归属行已有本机观测证据：不改绑，如实失败（修复只收敛从未被观测过的归组行）。
#[tokio::test]
async fn relink_refuses_to_rebind_a_row_with_recorded_evidence() {
    let store = Migrated::build().await;
    let facade = &store.facade;
    let id = "unbound-sub".to_owned();
    let owner = store.workspace_of(&id).await;
    let observed = facade
        .resolve_workspace(std::path::Path::new(
            &facade.load_session_meta(&id).await.unwrap().cwd,
        ))
        .await
        .unwrap();
    sqlx::query("UPDATE workspaces SET project_id = ?1, identity = 'recorded', discovery = ?2 WHERE id = ?3")
        .bind(observed.project_id.to_string())
        .bind(observed.discovery_snapshot.clone().unwrap())
        .bind(&owner)
        .execute(facade.local_pool())
        .await
        .unwrap();

    let error = facade.relink_unbound_session(&id).await.unwrap_err();
    assert!(
        matches!(
            error_kind(&error),
            SessionResourceErrorKind::Workspace(WorkspaceError::UnverifiedLegacyOwner)
        ),
        "{error}"
    );
    assert_eq!(
        store.workspace_of(&id).await,
        owner,
        "已有证据的归属不得被改写"
    );
    assert!(store.binding_rows(&id).await.is_empty());
}
