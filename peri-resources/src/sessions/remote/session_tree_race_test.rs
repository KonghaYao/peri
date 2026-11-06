//! 远端「删除会话树 / 撤销」与「创建子会话」之间的并发边界（本地 SQLite 扮演远端服务端）。
//!
//! 竞态窗口由 [`SqliteTransport::before_batch`] 固定：它在删除路径的只读快照之后、托管事务批
//! 开始之前执行一条测试语句，等价于另一个实例在两步之间提交。断言只看落库结果——不允许留下
//! 指向已删除父行的 `threads` 行。

use std::sync::Arc;

use peri_acp_types::session_resources::{
    ChildSnapshot, FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResourceErrorKind,
};
use peri_acp_types::store::InheritedContext;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

use super::cloud_tests::{synth_binding, synth_thread};
use super::generation::{ConnectionFactory, ConnectionGate};
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreId};
use super::schema_upgrade_tests::SqliteTransport;
use super::session_data::RemoteSessionData;
use crate::sessions::data::SessionDataPort;

const FROZEN: &str = r#"{"frozen":"race"}"#;
const CREATED_AT: &str = "2026-01-01T00:00:00Z";

struct Fixture {
    _directory: tempfile::TempDir,
    pool: SqlitePool,
    transport: Arc<SqliteTransport>,
    data: RemoteSessionData,
}

struct RaceFactory {
    transport: Arc<SqliteTransport>,
    access: StoreAccess,
    gate: Arc<ConnectionGate>,
}

#[async_trait::async_trait]
impl ConnectionFactory for RaceFactory {
    async fn connect(
        &self,
    ) -> peri_acp_types::session_resources::SessionResourceResult<RemoteStore> {
        Ok(RemoteStore::new(
            self.transport.clone(),
            self.access,
            self.gate.mint(),
            self.gate.clone(),
        ))
    }
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("remote-tree-race.db"))
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
    sqlx::query("INSERT INTO peri_store_meta VALUES (0, ?1, 'tree-race-test', ?2, 'now')")
        .bind(schema::REMOTE_SCHEMA_VERSION)
        .bind(schema::STORE_CONTRACT)
        .execute(&pool)
        .await
        .unwrap();

    let transport = Arc::new(SqliteTransport::for_test(pool.clone()));
    let gate = Arc::new(ConnectionGate::default());
    let access = StoreAccess::ReadWrite;
    let store = RemoteStore::new(transport.clone(), access, gate.mint(), gate.clone());
    let data = RemoteSessionData::with_connection_for_test(
        StoreId::mint(),
        store,
        Arc::new(RaceFactory {
            transport: transport.clone(),
            access,
            gate: gate.clone(),
        }),
        gate,
    );
    Fixture {
        _directory: directory,
        pool,
        transport,
        data,
    }
}

async fn seed_thread(pool: &SqlitePool, id: &str, parent: Option<&str>, frozen: Option<&str>) {
    sqlx::query(
        "INSERT INTO threads (id, cwd, created_at, updated_at, parent_thread_id,
            frozen_context, workspace_id)
         VALUES (?1, '/tmp', ?2, ?2, ?3, ?4, 'ws')",
    )
    .bind(id)
    .bind(CREATED_AT)
    .bind(parent)
    .bind(frozen)
    .execute(pool)
    .await
    .unwrap();
}

async fn count_threads(pool: &SqlitePool, ids: &[&str]) -> i64 {
    let mut total = 0;
    for id in ids {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM threads WHERE id = ?1")
            .bind(*id)
            .fetch_one(pool)
            .await
            .unwrap();
        total += row.0;
    }
    total
}

async fn count_orphans(pool: &SqlitePool) -> i64 {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM threads child
         WHERE child.parent_thread_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM threads parent WHERE parent.id = child.parent_thread_id)",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    row.0
}

fn child_snapshot(child: &str, parent: &str) -> ChildSnapshot {
    ChildSnapshot {
        target: NewSession {
            thread_id: synth_thread(child),
            created_at: CREATED_AT.to_owned(),
            meta: NewSessionMeta {
                title: Some(child.to_owned()),
                cwd: "/tmp".to_owned(),
                parent_thread_id: Some(synth_thread(parent)),
                hidden: true,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: synth_binding(),
            frozen: FrozenSnapshotBytes::new(FROZEN),
        },
        parent_id: synth_thread(parent),
        root_id: synth_thread(parent),
        inherited: InheritedContext::default(),
    }
}

fn late_child_insert(child: &str, parent: &str) -> String {
    format!(
        "INSERT INTO threads (id, cwd, created_at, updated_at, parent_thread_id, workspace_id)
         VALUES ('{child}', '/tmp', '{CREATED_AT}', '{CREATED_AT}', '{parent}', 'ws')"
    )
}

/// 正常路径：整棵子树（含并发提交的新 child）一起消失，不留孤儿。
#[tokio::test]
async fn delete_tree_removes_a_child_committed_after_the_tree_snapshot() {
    let fixture = fixture().await;
    seed_thread(&fixture.pool, "root", None, Some(FROZEN)).await;
    seed_thread(&fixture.pool, "child", Some("root"), Some(FROZEN)).await;

    // 另一个实例在「读树」之后、「删除批」之前提交了一个新 child。
    *fixture.transport.before_batch.lock().unwrap() =
        Some(late_child_insert("late-child", "child"));

    fixture
        .data
        .delete_tree(&"root".to_owned())
        .await
        .expect("tree deletion");

    assert_eq!(
        count_threads(&fixture.pool, &["root", "child", "late-child"]).await,
        0,
        "the late child belongs to the deleted subtree and must not survive"
    );
    assert_eq!(count_orphans(&fixture.pool).await, 0);
}

/// 撤销与 child 创建：撤销必须拒绝已经提交的子会话，不能先把父行删掉。
#[tokio::test]
async fn revoke_refuses_a_child_committed_after_the_child_count() {
    let fixture = fixture().await;
    seed_thread(&fixture.pool, "parent", None, Some(FROZEN)).await;

    *fixture.transport.before_batch.lock().unwrap() =
        Some(late_child_insert("late-child", "parent"));

    let error = fixture
        .data
        .revoke_unpublished_session(&"parent".to_owned())
        .await
        .expect_err("revocation with a published child must fail");

    assert!(matches!(
        error.kind(),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    assert_eq!(
        count_threads(&fixture.pool, &["parent", "late-child"]).await,
        2,
        "a rejected revocation must not delete the parent"
    );
    assert_eq!(count_orphans(&fixture.pool).await, 0);
}

/// child 创建与父行删除：父行在批开始前消失时，创建必须整体不生效并报 `NotFound`。
#[tokio::test]
async fn save_child_refuses_a_parent_deleted_before_its_batch() {
    let fixture = fixture().await;
    seed_thread(&fixture.pool, "root", None, Some(FROZEN)).await;

    *fixture.transport.before_batch.lock().unwrap() =
        Some("DELETE FROM threads WHERE id = 'root'".to_owned());

    let error = fixture
        .data
        .save_child(&child_snapshot("child", "root"))
        .await
        .expect_err("a child cannot be created under a deleted parent");

    assert!(
        matches!(error.kind(), SessionResourceErrorKind::NotFound),
        "expected NotFound, got {:?}",
        error.kind()
    );
    assert_eq!(count_threads(&fixture.pool, &["child"]).await, 0);
    assert_eq!(count_orphans(&fixture.pool).await, 0);
}

/// 正常路径不回归：合法 child 创建后仍能按父子关系删除整棵树。
#[tokio::test]
async fn child_creation_then_tree_deletion_round_trips() {
    let fixture = fixture().await;
    seed_thread(&fixture.pool, "root", None, Some(FROZEN)).await;

    fixture
        .data
        .save_child(&child_snapshot("child", "root"))
        .await
        .expect("child creation");
    fixture
        .data
        .delete_tree(&"root".to_owned())
        .await
        .expect("tree deletion");

    assert_eq!(count_threads(&fixture.pool, &["root", "child"]).await, 0);
    assert_eq!(count_orphans(&fixture.pool).await, 0);
}
