//! 远端长驻适配器的本机缓存回收（issue #473）。
//!
//! 本地 SQLite 扮演远端服务端；只断言本机缓存，不断言网络。
//!
//! | 用例 | 断言的事实 |
//! | --- | --- |
//! | roots | 删除会话树后，该树 thread → root 的解析缓存被回收，其他树保留 |
//! | observations | 删除会话树后，该树工作目录的观测被回收；共享目录的有效绑定仍可验证与恢复 |

use std::sync::Arc;

use peri_acp_types::session_resources::{
    BindingRecheck, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResourceErrorKind,
    SessionResources,
};
use peri_acp_types::thread::CancelPolicy;
use peri_acp_types::workspace::SessionBinding;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use super::environment::RemoteWorkspaceEnvironment;
use super::execution::RemoteExecution;
use super::generation::{ConnectionFactory, ConnectionGate};
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreId};
use super::schema_upgrade_tests::SqliteTransport;
use super::session_data::RemoteSessionData;
use crate::sessions::data::SessionDataPort;
use crate::sessions::local_port::LocalExecutionPort;
use crate::sessions::SessionResourcesImpl;

const CREATED_AT: &str = "2026-01-01T00:00:00Z";

struct Fixture {
    _directory: tempfile::TempDir,
    pool: SqlitePool,
    transport: Arc<SqliteTransport>,
}

struct TestFactory {
    transport: Arc<SqliteTransport>,
    gate: Arc<ConnectionGate>,
}

#[async_trait::async_trait]
impl ConnectionFactory for TestFactory {
    async fn connect(
        &self,
    ) -> peri_acp_types::session_resources::SessionResourceResult<RemoteStore> {
        Ok(RemoteStore::new(
            self.transport.clone(),
            StoreAccess::ReadWrite,
            self.gate.mint(),
            self.gate.clone(),
        ))
    }
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("remote-retention.db"))
                .create_if_missing(true)
                .foreign_keys(false),
        )
        .await
        .unwrap();
    for statement in super::session_schema::initialization_plan() {
        sqlx::query(statement.sql).execute(&pool).await.unwrap();
    }
    sqlx::query(schema::CREATE_STORE_META_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(super::ledger::CREATE_OP_LEDGER_SQL)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'retention-test', ?2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION)
        .bind(schema::STORE_CONTRACT)
        .execute(&pool)
        .await
        .unwrap();
    let transport = Arc::new(SqliteTransport::for_test(pool.clone()));
    Fixture {
        _directory: directory,
        pool,
        transport,
    }
}

fn remote_adapter(transport: &Arc<SqliteTransport>, machine_id: String) -> Arc<RemoteSessionData> {
    let gate = Arc::new(ConnectionGate::default());
    let store = RemoteStore::new(
        transport.clone(),
        StoreAccess::ReadWrite,
        gate.mint(),
        gate.clone(),
    );
    let mut data = RemoteSessionData::with_connection_for_test(
        StoreId::mint(),
        store,
        Arc::new(TestFactory {
            transport: transport.clone(),
            gate: gate.clone(),
        }),
        gate,
    );
    data.machine_id = machine_id;
    Arc::new(data)
}

fn facade(
    data: Arc<RemoteSessionData>,
    execution: Arc<RemoteExecution>,
) -> Arc<SessionResourcesImpl> {
    let local: Arc<dyn LocalExecutionPort> = execution;
    let data: Arc<dyn SessionDataPort> = data;
    Arc::new(SessionResourcesImpl::from_ports(
        data,
        local,
        crate::sessions::resources::SessionDataHome::RemoteStore,
    ))
}

async fn seed_thread(pool: &SqlitePool, id: &str, parent: Option<&str>) {
    sqlx::query(
        "INSERT INTO threads (id, cwd, created_at, updated_at, parent_thread_id,
            frozen_context, workspace_id)
         VALUES (?1, '/tmp', ?2, ?2, ?3, '{}', 'ws')",
    )
    .bind(id)
    .bind(CREATED_AT)
    .bind(parent)
    .execute(pool)
    .await
    .unwrap();
}

fn session_input(thread_id: &str, cwd: &std::path::Path, binding: SessionBinding) -> NewSession {
    NewSession {
        thread_id: thread_id.to_owned(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: NewSessionMeta {
            title: None,
            cwd: cwd.to_str().unwrap().to_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: None,
        },
        binding,
        frozen: FrozenSnapshotBytes::new("{}"),
    }
}

#[tokio::test]
async fn deleting_a_subtree_reclaims_its_cached_roots_and_keeps_other_trees() {
    let fixture = fixture().await;
    seed_thread(&fixture.pool, "ret-root", None).await;
    seed_thread(&fixture.pool, "ret-child", Some("ret-root")).await;
    seed_thread(&fixture.pool, "ret-grandchild", Some("ret-child")).await;
    seed_thread(&fixture.pool, "keep-root", None).await;
    let data = remote_adapter(&fixture.transport, "machine".to_owned());

    for id in ["ret-root", "ret-child", "ret-grandchild", "keep-root"] {
        data.session_root(&id.to_owned()).await.unwrap();
    }
    assert_eq!(data.cached_roots().await.len(), 4);

    // 删除中间节点：后代缓存的 root 位于子树之外（ret-grandchild → ret-root）。
    data.delete_tree(&"ret-child".to_owned()).await.unwrap();

    let roots = data.cached_roots().await;
    assert!(!roots.contains_key("ret-child"));
    assert!(!roots.contains_key("ret-grandchild"));
    assert_eq!(
        roots.get("ret-root").map(String::as_str),
        Some("ret-root"),
        "子树之外的根必须保留"
    );
    assert_eq!(
        roots.get("keep-root").map(String::as_str),
        Some("keep-root"),
        "其他会话树的缓存必须保留"
    );

    // 删除根：自身条目同样回收。
    data.delete_tree(&"ret-root".to_owned()).await.unwrap();
    let roots = data.cached_roots().await;
    assert!(!roots.contains_key("ret-root"));
    assert_eq!(
        roots.get("keep-root").map(String::as_str),
        Some("keep-root"),
        "其他会话树的缓存必须保留"
    );
}

#[tokio::test]
async fn deleted_session_directories_release_observations_and_other_bindings_still_validate() {
    let fixture = fixture().await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let root = workspace_dir.path().to_path_buf();
    let machine = uuid::Uuid::new_v4().to_string();
    let environment =
        RemoteWorkspaceEnvironment::virtual_workspace(&machine, root.clone()).unwrap();
    let data = remote_adapter(&fixture.transport, machine);
    let execution = Arc::new(RemoteExecution::new_in_environment(
        data.clone(),
        false,
        environment,
    ));
    let facade = facade(data, execution.clone());

    let root_workspace = facade.resolve_workspace(&root).await.unwrap();
    let sub = root.join("sub");
    let sub_workspace = facade.resolve_workspace(&sub).await.unwrap();
    let root_id = "retention-root".to_owned();
    let sub_id = "retention-sub".to_owned();
    let sibling_id = "retention-sibling".to_owned();
    facade
        .create_session(&session_input(
            &root_id,
            &root,
            SessionBinding::from_workspace(&root_workspace),
        ))
        .await
        .unwrap();
    facade
        .create_session(&session_input(
            &sub_id,
            &sub,
            SessionBinding::from_workspace(&sub_workspace),
        ))
        .await
        .unwrap();
    // 与 root_id 共享 cwd 的另一个会话：按工作目录回收会连它的观测一起清掉。
    facade
        .create_session(&session_input(
            &sibling_id,
            &root,
            SessionBinding::from_workspace(&root_workspace),
        ))
        .await
        .unwrap();
    assert_eq!(execution.observation_cwds().len(), 2);

    facade.delete_session_tree(&root_id).await.unwrap();

    let observed = execution.observation_cwds();
    assert!(
        !observed.contains(&root),
        "被删会话的工作目录观测必须回收: {observed:?}"
    );
    assert!(
        observed.contains(&sub),
        "其他会话的观测必须保留: {observed:?}"
    );
    // 回收的是本机观测缓存，不是持久绑定：共享目录的会话仍能按保存的证据重新观测。
    let sibling = facade
        .validate_bound_workspace(&sibling_id, BindingRecheck::Full)
        .await
        .unwrap();
    assert_eq!(sibling.cwd, root);
    assert_eq!(sibling.workspace_id, root_workspace.workspace_id);
    assert!(
        execution.observation_cwds().contains(&root),
        "恢复时必须按保存的绑定证据重新观测"
    );
    let restored = facade
        .validate_bound_workspace(&sub_id, BindingRecheck::Full)
        .await
        .unwrap();
    assert_eq!(restored.cwd, sub);
    assert_eq!(restored.workspace_id, sub_workspace.workspace_id);
    assert!(matches!(
        facade
            .validate_bound_workspace(&root_id, BindingRecheck::Full)
            .await
            .unwrap_err()
            .kind(),
        SessionResourceErrorKind::NotFound
    ));
}
