//! `threads` 行删除的不变量：子行清理是**显式**的，不依赖 SQLite 外键级联。
//!
//! 两个方向各挡一类回归：
//!
//! - **schema 交叉核对**（`test_every_thread_referencing_table_is_deleted_explicitly`）：
//!   期望集合从运行库的**真实 schema** 派生（哪些表的外键指向 `threads`），再与生产声明
//!   [`THREAD_CHILD_DELETES`] 比对。新增一张 `REFERENCES threads(...)` 的子表会让它失败，
//!   直到删除逻辑跟上。
//! - **行为证明**（三个 `*_without_cascade` 测试）：在 `PRAGMA foreign_keys = OFF` 的 pool
//!   上跑生产的删除路径，断言删除后没有一张派生子表留下孤儿行。级联被真正关掉，这条断言
//!   因此只可能由显式删除满足——它就是远端执行器（无条件级联）上的同一份逻辑。
//!
//! 为什么必须这样测：本机 schema 声明了 `ON DELETE CASCADE`，谁都能「看出」显式删除多余
//! ——看错了。远端执行器提供不了级联（母 issue §9.28 的例外 2/3/4：默认读数为 0、跨连接
//! 共享、无 `foreign_key_check` 等价物），级联在本机只是
//! 安全网。删掉显式删除语句，本机外键打开时照样全绿，只有这里会红。

use peri_acp_types::session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding, SESSION_BINDING_VERSION};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tempfile::TempDir;

use super::session_rows::THREAD_CHILD_DELETES;
use super::*;
use crate::sessions::data::SessionDataPort;

// ─── 夹具：同一份 schema，关掉外键强制的执行器 ────────────────────────────────

struct NoCascade {
    store: SqliteThreadStore,
    data: SqliteSessionData,
    directory: TempDir,
}

/// 先由生产 open 建好 schema（含 `ON DELETE CASCADE` 声明），再关掉这个 pool，用**同一个
/// 文件**、但 `PRAGMA foreign_keys = OFF` 的 pool 打开它——远端 Turso 默认读数的可复现替身。
///
/// 不为测试降低生产可见性：`SqliteSessionDatabase::new` / `SqliteSessionData::new` 对本模块
/// （`sqlite_store` 的后代）本就可见。
async fn without_cascade() -> NoCascade {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("threads.db");
    SqliteThreadStore::new(&path).await.unwrap().close().await;
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(false)
                .pragma("journal_mode", "WAL")
                .pragma("foreign_keys", "OFF"),
        )
        .await
        .unwrap();
    // 夹具自证：pool 上的读数必须是 0，否则下面每条断言都可能是级联替我们过的。
    let (reading,): (i64,) = sqlx::query_as("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(reading, 0, "夹具必须跑在关掉外键强制的执行器上");
    let database = Arc::new(SqliteSessionDatabase::new(
        pool,
        false,
        tokio::fs::canonicalize(&path).await.unwrap(),
    ));
    NoCascade {
        store: SqliteThreadStore {
            database: Arc::clone(&database),
        },
        data: SqliteSessionData::new(database),
        directory,
    }
}

/// 一条会话 = `threads` 行 + `session_bindings` 行 + 两条 `messages` 行，覆盖 schema 里
/// 当前全部指向 `threads` 的子表。
async fn seed(fixture: &NoCascade, id: &str, parent: Option<&str>) {
    let workspace: ResolvedWorkspace = fixture
        .store
        .database
        .resolve_workspace_impl(fixture.directory.path())
        .await
        .unwrap();
    let input = NewSession {
        thread_id: id.to_owned(),
        created_at: "2026-09-27T00:00:00Z".to_owned(),
        meta: NewSessionMeta {
            title: Some(format!("session {id}")),
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: parent.map(str::to_owned),
            hidden: parent.is_some(),
            cancel_policy: Default::default(),
            snapshot_at_message_id: None,
        },
        binding: SessionBinding {
            schema_version: SESSION_BINDING_VERSION,
            revision: 1,
            project_id: workspace.project_id,
            workspace_id: workspace.workspace_id,
            cwd_relative_to_workspace: workspace.relative_cwd.clone(),
        },
        frozen: FrozenSnapshotBytes::new(format!(r#"{{"v":1,"id":"{id}"}}"#)),
    };
    fixture.data.save_new_session(&input).await.unwrap();
    fixture
        .data
        .append_history(
            &id.to_owned(),
            &[
                PersistedPayload::Message(BaseMessage::human(format!("{id} 1"))),
                PersistedPayload::Message(BaseMessage::human(format!("{id} 2"))),
            ],
        )
        .await
        .unwrap();
}

// ─── 期望集合：从真实 schema 派生，不从生产语句反推 ───────────────────────────

/// 「行会随 `threads` 行一起消失」的子表：`(表名, 外键列)`。
///
/// 唯一来源是运行库的 schema（`pragma_foreign_key_list`）。不拿
/// [`THREAD_CHILD_DELETES`] 反推——那样测试只是实现自己的镜像，什么也挡不住。
async fn schema_child_tables(pool: &SqlitePool) -> Vec<(String, String)> {
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut child_tables = Vec::new();
    for (table,) in tables {
        let references: Vec<(String, String)> =
            sqlx::query_as("SELECT \"table\", \"from\" FROM pragma_foreign_key_list(?)")
                .bind(&table)
                .fetch_all(pool)
                .await
                .unwrap();
        for (parent, column) in references {
            if parent == "threads" {
                child_tables.push((table.clone(), column));
            }
        }
    }
    child_tables.sort();
    child_tables
}

/// 一条 thread 在某张表里还剩多少行。
async fn rows_for(pool: &SqlitePool, table: &str, column: &str, id: &str) -> i64 {
    let (count,): (i64,) = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM \"{table}\" WHERE \"{column}\" = ?1"
    )))
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    count
}

/// 删除前：每一张派生子表都必须有这些 thread 的行，否则「删完没有孤儿」是句空话。
async fn assert_child_rows_present(pool: &SqlitePool, tables: &[(String, String)], ids: &[&str]) {
    for (table, column) in tables {
        for id in ids {
            assert!(
                rows_for(pool, table, column, id).await > 0,
                "夹具必须让子表 {table} 有 {id} 的行：新增指向 threads 的子表后请在这里补种子数据"
            );
        }
    }
}

/// 删除后：没有任何一张派生子表留下指向已删 thread 的孤儿行。
async fn assert_no_orphans(pool: &SqlitePool, tables: &[(String, String)], ids: &[&str]) {
    for (table, column) in tables {
        for id in ids {
            assert_eq!(
                rows_for(pool, table, column, id).await,
                0,
                "{table}.{column} 里还有 {id} 的孤儿行：删除路径借了 `ON DELETE CASCADE`，\
                 而远端执行器没有级联，请改成显式删除（见 THREAD_CHILD_DELETES）"
            );
        }
    }
}

// ─── 期望集合 vs 生产语句 ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_every_thread_referencing_table_is_deleted_explicitly() {
    let fixture = without_cascade().await;
    let derived = schema_child_tables(&fixture.store.database.pool).await;
    assert!(
        !derived.is_empty(),
        "schema 派生结果为空：探测本身失效，下面的交叉核对会变成空转"
    );

    for (table, column) in &derived {
        let declared = THREAD_CHILD_DELETES.iter().find(|(name, _)| name == table);
        let Some((_, statement)) = declared else {
            panic!(
                "{table} 有指向 threads 的外键：它的行会随 threads 行消失，\
                 THREAD_CHILD_DELETES 必须显式删除它（远端执行器没有级联可借）"
            );
        };
        assert!(
            statement.contains(table.as_str()),
            "声明了 {table}，语句却删的是别的表：{statement}"
        );
        assert!(
            statement.contains(column.as_str()),
            "{table} 的外键列是 {column}，删除语句必须按该列删：{statement}"
        );
    }

    for (table, _) in THREAD_CHILD_DELETES {
        assert!(
            derived.iter().any(|(name, _)| name == table),
            "THREAD_CHILD_DELETES 里的 {table} 没有指向 threads 的外键：表名写错，\
             或这份清单被用来装非外键子表（那需要连同本断言一起改）"
        );
    }
}

// ─── 行为证明：三条生产删除路径在无级联执行器上都不留孤儿 ─────────────────────

/// 数据面 `delete_tree`（整棵子树）。
#[tokio::test]
async fn test_data_delete_tree_leaves_no_orphans_without_cascade() {
    let fixture = without_cascade().await;
    let parent = "s-parent";
    let child = "s-child";
    seed(&fixture, parent, None).await;
    seed(&fixture, child, Some(parent)).await;
    let pool = fixture.store.database.pool.clone();
    let tables = schema_child_tables(&pool).await;
    assert_child_rows_present(&pool, &tables, &[parent, child]).await;

    fixture.data.delete_tree(&parent.to_owned()).await.unwrap();

    assert_no_orphans(&pool, &tables, &[parent, child]).await;
    for id in [parent, child] {
        assert_eq!(rows_for(&pool, "threads", "id", id).await, 0, "{id} 未删除");
    }
}

/// 数据面 `revoke_unpublished_session`（单条未发布会话）。
///
/// 撤销的对象是**未提交 frozen** 的草稿：已提交快照的会话不是「未发布创建」，撤销必须
/// 拒绝（见 `test_revoke_refuses_a_committed_session`），所以这里先删掉种子写入的 frozen
/// 值，让判据回到草稿态。
#[tokio::test]
async fn test_revoke_unpublished_session_leaves_no_orphans_without_cascade() {
    let fixture = without_cascade().await;
    let id = "s-revoked";
    seed(&fixture, id, None).await;
    sqlx::query("UPDATE threads SET frozen_context = NULL WHERE id = ?1")
        .bind(id)
        .execute(&fixture.store.database.pool)
        .await
        .unwrap();
    let pool = fixture.store.database.pool.clone();
    let tables = schema_child_tables(&pool).await;
    assert_child_rows_present(&pool, &tables, &[id]).await;

    fixture
        .data
        .revoke_unpublished_session(&id.to_owned())
        .await
        .unwrap();

    assert_no_orphans(&pool, &tables, &[id]).await;
    assert_eq!(rows_for(&pool, "threads", "id", id).await, 0, "{id} 未删除");
}

/// 迁移桥 `SqliteThreadStore::delete_thread`（`ThreadStore` 转发）。
#[tokio::test]
async fn test_bridge_delete_thread_leaves_no_orphans_without_cascade() {
    let fixture = without_cascade().await;
    let id = "s-bridged";
    seed(&fixture, id, None).await;
    let pool = fixture.store.database.pool.clone();
    let tables = schema_child_tables(&pool).await;
    assert_child_rows_present(&pool, &tables, &[id]).await;

    // 桥的删除走执行准入：有绑定行的会话是「有主」的，按生产语义先取得所有权。
    let facts = fixture
        .store
        .database
        .local_session_facts(&id.to_owned())
        .await
        .unwrap();
    let _lease = fixture
        .store
        .database
        .acquire_execution_lease_impl(&id.to_owned(), &facts)
        .await
        .unwrap();

    fixture.store.delete_thread(&id.to_owned()).await.unwrap();

    assert_no_orphans(&pool, &tables, &[id]).await;
    assert_eq!(rows_for(&pool, "threads", "id", id).await, 0, "{id} 未删除");
}
