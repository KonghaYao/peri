//! 形状声明的验收：声明必须与真实新建库一致，且能拦住已发生的漂移。
//!
//! 这是「第二份形状事实源」的守门测试——`CURRENT_TABLES` 与 canonical DDL 的一致性由
//! [`declared_current_shape_matches_a_fresh_database`] 断言，DDL 改动而声明未跟上会失败。
#![cfg(not(target_os = "emscripten"))]

use super::*;
use crate::sessions::canonical;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{AssertSqlSafe, Connection, SqliteConnection};

fn current_table(name: &str) -> Option<&'static TableShape> {
    CURRENT_TABLES.iter().find(|table| table.name == name)
}

fn input_table(name: &str) -> Option<&'static InputTable> {
    INPUT_TABLES.iter().find(|table| table.name == name)
}

fn dropped_table(name: &str) -> Option<&'static DroppedTable> {
    DROPPED_TABLES.iter().find(|table| table.shape.name == name)
}

/// 正式发布（≤10）用过、当前形状退役的 `threads` 列；必须仍在迁移输入的列上界里。
const RETIRED_THREAD_COLUMNS: &[&str] = &["cached_context", "context_cache_epoch"];

async fn empty_database() -> (tempfile::TempDir, SqliteConnection) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shape.db");
    let connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    (directory, connection)
}

async fn execute(connection: &mut SqliteConnection, statement: &str) {
    // 语句文本来自测试常量与 `canonical` 的静态清单，不含外部输入。
    sqlx::raw_sql(AssertSqlSafe(statement.to_owned()))
        .execute(connection)
        .await
        .unwrap();
}

async fn fresh_database() -> (tempfile::TempDir, SqliteConnection) {
    let (directory, mut connection) = empty_database().await;
    for statement in canonical::CREATE_TABLES
        .iter()
        .chain(canonical::CREATE_INDEXES)
    {
        execute(&mut connection, statement).await;
    }
    (directory, connection)
}

/// 声明 == 真实新建库（8 张表逐列形状）。
#[tokio::test]
async fn declared_current_shape_matches_a_fresh_database() {
    let (_directory, mut connection) = fresh_database().await;
    let tables = read_local_columns(&mut connection, CURRENT_COLUMNS_SQL)
        .await
        .unwrap();
    check_current_shape(&tables).unwrap();
    // 声明的表集合就是读回的表集合：多读到的表说明声明漏覆盖。
    for name in tables.keys() {
        assert!(current_table(name).is_some(), "undeclared table {name}");
    }
    assert_eq!(tables.len(), CURRENT_TABLES.len());
}

/// 可选表（凭证、关闭意图）缺表不是漂移，而必需表缺表必须失败——两条容忍线的分界。
#[tokio::test]
async fn optional_tables_may_be_absent_but_mandatory_tables_may_not() {
    let (_directory, mut connection) = fresh_database().await;
    for table in ["mcp_oauth_credentials", "session_close_intents"] {
        execute(&mut connection, &format!("DROP TABLE {table}")).await;
    }
    let tables = read_local_columns(&mut connection, CURRENT_COLUMNS_SQL)
        .await
        .unwrap();
    check_current_shape(&tables).unwrap();
    execute(&mut connection, "DROP TABLE machines").await;
    let tables = read_local_columns(&mut connection, CURRENT_COLUMNS_SQL)
        .await
        .unwrap();
    let error = check_current_shape(&tables).unwrap_err();
    assert!(error.contains("machines"), "{error}");
}

/// 可选表存在时仍必须同形：同名异形的凭证表是别的对象，不能因为「可选」而放行。
#[tokio::test]
async fn a_foreign_optional_table_is_still_refused() {
    let (_directory, mut connection) = fresh_database().await;
    execute(&mut connection, "DROP TABLE mcp_oauth_credentials").await;
    execute(
        &mut connection,
        "CREATE TABLE mcp_oauth_credentials (server_key TEXT PRIMARY KEY, blob TEXT NOT NULL)",
    )
    .await;
    let tables = read_local_columns(&mut connection, CURRENT_COLUMNS_SQL)
        .await
        .unwrap();
    let error = check_current_shape(&tables).unwrap_err();
    assert!(error.contains("mcp_oauth_credentials"), "{error}");
}

/// 迁移实现如果又把 `workspace_id` 建成可空（历史上发生过），严格判定必须失败。
#[test]
fn nullable_workspace_id_is_refused_by_the_strict_declaration() {
    let table = current_table("threads").unwrap();
    let actual: Vec<ActualColumn> = table
        .columns
        .iter()
        .map(|name| {
            let not_null = table.not_null.contains(name) && *name != "workspace_id";
            ActualColumn::new(*name, not_null, i64::from(table.primary_key.contains(name)))
        })
        .collect();
    let error = check_strict(table, &actual).unwrap_err();
    assert!(error.contains("workspace_id"), "{error}");
}

/// 缺表（这里是 `machines`）必须失败，而不是被当成空形状放行。
#[test]
fn missing_table_is_refused() {
    let actual = vec![ActualColumn::new("id", false, 1)];
    let table = current_table("machines").unwrap();
    assert!(check_strict(table, &actual).is_err());
}

/// 压缩前形状的列序列缺失即失败：`workspace_id` / `archived` 是当前形状才有的列。
#[test]
fn current_only_columns_are_not_expected_in_the_input_shape() {
    let table = input_table("threads").unwrap();
    let actual: Vec<ActualColumn> = THREADS_COLUMNS
        .iter()
        .copied()
        .filter(|name| *name != "workspace_id" && *name != "archived")
        .map(|name| ActualColumn::new(name, false, 0))
        .collect();
    check_input(table, &actual).unwrap();
}

/// 退役缓存列仍在输入上界里（`removal_plan` 负责显式删除，判定不因它们存在而拒绝）。
#[test]
fn retired_thread_columns_stay_inside_the_input_upper_bound() {
    let allowed = input_table("threads").unwrap().allowed.unwrap();
    for column in RETIRED_THREAD_COLUMNS {
        assert!(allowed.contains(column), "{column}");
    }
}

/// 正式发布 10 的 `threads` 形状（含后来退役的两个缓存列）必须被接受。
#[tokio::test]
async fn published_ten_input_shape_is_accepted() {
    let (_directory, mut connection) = empty_database().await;
    execute(&mut connection, PUBLISHED_TEN_DDL).await;
    let tables = read_local_columns(&mut connection, INPUT_COLUMNS_SQL)
        .await
        .unwrap();
    check_input_shape(&tables).unwrap();
}

/// 混合形状（登记语义的 `workspaces` 里混进了当前形状的 `machine_id`）必须被拒绝：
/// 搬运会重建这张表，未知列的值没有去处。
#[tokio::test]
async fn mixed_input_shape_is_refused() {
    let (_directory, mut connection) = empty_database().await;
    execute(
        &mut connection,
        &format!(
            "{PUBLISHED_TEN_DDL}
        ALTER TABLE workspaces ADD COLUMN machine_id TEXT;"
        ),
    )
    .await;
    let tables = read_local_columns(&mut connection, INPUT_COLUMNS_SQL)
        .await
        .unwrap();
    let error = check_input_shape(&tables).unwrap_err();
    assert!(error.contains("machine_id"), "{error}");
}

/// 删除声明 == 压缩前的建表文本（`canonical::V10_*`）：声明与真实 DDL 的一致性同样由测试守门，
/// 判定不是另写的一份形状事实。
#[tokio::test]
async fn declared_dropped_shapes_match_the_legacy_ddl() {
    let (_directory, mut connection) = empty_database().await;
    execute(
        &mut connection,
        canonical::V10_CREATE_ENVIRONMENTS_TABLE_SQL,
    )
    .await;
    execute(
        &mut connection,
        canonical::V10_CREATE_OAUTH_CREDENTIALS_TABLE_SQL,
    )
    .await;
    let tables = read_local_columns(&mut connection, INPUT_COLUMNS_SQL)
        .await
        .unwrap();
    let plan = dropped_plan(&tables).unwrap();
    for table in DROPPED_TABLES {
        assert!(plan.contains(&table.drop_sql), "{}", table.shape.name);
    }
    assert_eq!(plan.len(), DROPPED_TABLES.len());
}

/// 缺表时只跳过允许缺表的删除：`session_environments` 由迁移自己补齐，删除照常下发
/// （`IF EXISTS` 让「移植为规划而建、远端输入整表缺失」两种情况共用一条语句）。
#[tokio::test]
async fn missing_legacy_tables_skip_only_the_optional_drop() {
    let (_directory, mut connection) = empty_database().await;
    let tables = read_local_columns(&mut connection, INPUT_COLUMNS_SQL)
        .await
        .unwrap();
    let plan = dropped_plan(&tables).unwrap();
    assert_eq!(plan, vec!["DROP TABLE IF EXISTS session_environments"]);
}

/// 同名异形的表不是迁移要删的对象：判定失败，且**拿不到**删除语句。
#[tokio::test]
async fn foreign_table_with_a_legacy_name_is_refused() {
    let (_directory, mut connection) = empty_database().await;
    execute(
        &mut connection,
        "CREATE TABLE mcp_oauth_credentials (
        principal_id TEXT PRIMARY KEY, credentials_blob TEXT NOT NULL, updated_at TEXT NOT NULL)",
    )
    .await;
    let tables = read_local_columns(&mut connection, INPUT_COLUMNS_SQL)
        .await
        .unwrap();
    let error = dropped_plan(&tables).unwrap_err();
    assert!(error.contains("mcp_oauth_credentials"), "{error}");
    // 判定的是删除动作：库里的别的同名表既不删也不放行。
    assert!(dropped_table("mcp_oauth_credentials").is_some());
}

/// 新建库的索引名集合 == `canonical::CREATE_INDEXES` 声明的索引名集合（索引不在形状判定里，
/// 由这条测试与迁移终点测试守住「建齐」）。
#[tokio::test]
async fn a_fresh_database_carries_every_canonical_index() {
    let (_directory, mut connection) = fresh_database().await;
    let actual: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_schema WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .fetch_all(&mut connection)
            .await
            .unwrap();
    let declared: Vec<String> = canonical::CREATE_INDEXES
        .iter()
        .map(|statement| declared_index_name(statement))
        .collect();
    let mut actual: Vec<String> = actual.into_iter().map(|(name,)| name).collect();
    let mut declared = declared;
    actual.sort();
    declared.sort();
    assert_eq!(actual, declared);
}

/// 从 `CREATE INDEX IF NOT EXISTS <name> ON ...` 里取出索引名（本模块的 DDL 形态固定）。
fn declared_index_name(statement: &str) -> String {
    statement
        .split_whitespace()
        .skip_while(|token| *token != "EXISTS")
        .nth(1)
        .expect("canonical index statement must name the index")
        .to_owned()
}

/// `threads` 的重建语句（暂存表 DDL + 两条搬运 INSERT）必须逐项搬运当前形状的列序列。
///
/// 这四处列清单（`CREATE_THREADS_TABLE_SQL` + 三处重建语句）都是手写的，任何一处漏改都只在
/// 重建当场才暴露；这里把三处重建语句绑到 `CURRENT_TABLES`，`CREATE_THREADS_TABLE_SQL` 那条由
/// [`declared_current_shape_matches_a_fresh_database`] 绑定，四处因此同源。
#[test]
fn rebuild_statements_carry_the_canonical_threads_columns() {
    let declared: Vec<String> = current_table("threads")
        .unwrap()
        .columns
        .iter()
        .map(|column| (*column).to_owned())
        .collect();
    for (label, statement) in [
        (
            "重建暂存表 DDL",
            canonical::CREATE_REBUILD_THREADS_TABLE_SQL,
        ),
        ("搬入暂存表", canonical::INSERT_THREADS_INTO_REBUILD_SQL),
        ("搬回 threads", canonical::INSERT_REBUILD_INTO_THREADS_SQL),
    ] {
        assert_eq!(declared_column_names(statement), declared, "{label}");
    }
}

/// 取语句里第一个括号组的顶层逗号项的首个词：建表语句得到列名，插入语句得到目标列名。
fn declared_column_names(statement: &str) -> Vec<String> {
    let open = statement
        .find('(')
        .expect("statement must open a column list");
    let mut depth = 0usize;
    let mut body = None;
    for (offset, character) in statement[open..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    body = Some(&statement[open + 1..open + offset]);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = body.expect("statement must close its column list");
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (offset, character) in body.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            // 列约束里的逗号（`CHECK(archived IN (0, 1))`）留在同一项里。
            ',' if depth == 0 => {
                items.push(body[start..offset].trim());
                start = offset + 1;
            }
            _ => {}
        }
    }
    items.push(body[start..].trim());
    items
        .into_iter()
        .map(|item| {
            item.split_whitespace()
                .next()
                .expect("column item must name a column")
                .to_owned()
        })
        .collect()
}

/// 正式发布 10 的形状快照（手写，不引用 `canonical` 的当前清单）：本机 canonical 在
/// 该版本上的表形状 + 后来退役的两个缓存列。这是迁移输入判定必须接受的形状。
const PUBLISHED_TEN_DDL: &str = "CREATE TABLE threads (
    id TEXT PRIMARY KEY, title TEXT, cwd TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, message_count INTEGER NOT NULL DEFAULT 0,
    parent_thread_id TEXT, snapshot_at_message_id TEXT, hidden BOOLEAN NOT NULL DEFAULT 0,
    cancel_policy TEXT NOT NULL DEFAULT 'cascade', config TEXT, cached_context TEXT,
    frozen_context TEXT, inherited_context TEXT, agent_status TEXT NOT NULL DEFAULT 'active',
    context_cache_epoch INTEGER NOT NULL DEFAULT 0);
CREATE TABLE messages (
    message_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL, content TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT 0, excluded BOOLEAN NOT NULL DEFAULT 0, projection TEXT);
CREATE TABLE projects (
    id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
    UNIQUE(locator, object_identity));
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
    UNIQUE(root, root_identity), UNIQUE(id, project_id));
CREATE TABLE session_bindings (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL, workspace_id TEXT NOT NULL, relative_cwd TEXT NOT NULL,
    FOREIGN KEY(workspace_id, project_id) REFERENCES workspaces(id, project_id));
CREATE TABLE session_environments (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
    machine_id TEXT NOT NULL);
CREATE TABLE mcp_oauth_credentials (
    principal_id TEXT NOT NULL, machine_id TEXT NOT NULL, server_key TEXT NOT NULL,
    credentials_blob TEXT NOT NULL, updated_at TEXT NOT NULL,
    PRIMARY KEY (principal_id, machine_id, server_key));
CREATE INDEX idx_messages_thread_id ON messages(thread_id);
CREATE INDEX idx_bindings_project ON session_bindings(project_id, thread_id);
CREATE INDEX idx_bindings_workspace ON session_bindings(workspace_id, relative_cwd, thread_id);
CREATE INDEX idx_threads_updated ON threads(updated_at DESC, id DESC) WHERE hidden = 0 AND message_count > 0;";
