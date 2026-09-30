//! 会话行的私有写入原语：`threads` 行与 canonical binding 行的插入与删除。
//!
//! 数据面（[`super::session_data`]）与迁移桥（[`super::SqliteThreadStore`]）共用同一
//! 份列清单、绑定校验与删除语句，避免同一张表的 INSERT/DELETE 与同一套绑定规则在两处
//! 各自维护。这里只做同事务内的行写入，不决定准入、不落锚点。

use anyhow::Result;
use peri_acp_types::workspace::SessionBinding;
use sqlx::SqliteConnection;

use crate::sessions::canonical;

/// 新建 `threads` 行的显式输入；派生字段（`message_count`、时间戳）由调用方给定，
/// 不在本层读时钟。
pub(super) struct ThreadRowInsert<'a> {
    pub id: &'a str,
    pub title: Option<&'a str>,
    pub cwd: &'a str,
    pub created_at: &'a str,
    pub updated_at: &'a str,
    pub message_count: i64,
    pub parent_thread_id: Option<&'a str>,
    pub snapshot_at_message_id: Option<&'a str>,
    pub hidden: bool,
    pub cancel_policy: &'a str,
    pub config: Option<&'a str>,
    pub agent_status: &'a str,
    pub frozen_context: Option<&'a str>,
}

/// 插入一条 `threads` 行；`cached_context` 与 `context_cache_epoch` 从缺省值起步
/// （派生缓存由行为在失效时清空，不在这里给值）。
pub(super) async fn insert_thread_row(
    connection: &mut SqliteConnection,
    row: &ThreadRowInsert<'_>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
            parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config, cached_context,
            frozen_context, agent_status, context_cache_epoch)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12, ?13, 0)",
    )
    .bind(row.id)
    .bind(row.title)
    .bind(row.cwd)
    .bind(row.created_at)
    .bind(row.updated_at)
    .bind(row.message_count)
    .bind(row.parent_thread_id)
    .bind(row.snapshot_at_message_id)
    .bind(row.hidden)
    .bind(row.cancel_policy)
    .bind(row.config)
    .bind(row.frozen_context)
    .bind(row.agent_status)
    .execute(&mut *connection)
    .await?;
    insert_environment_row(connection, row.id, row.parent_thread_id).await?;
    Ok(())
}

pub(super) async fn insert_environment_row(
    connection: &mut SqliteConnection,
    id: &str,
    parent: Option<&str>,
) -> Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(canonical::INSERT_ENVIRONMENT_SQL))
        .bind(id)
        .bind(parent)
        .bind(crate::sessions::machine::current()?)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// 插入 canonical binding 行。
///
/// 绑定身份由调用方给出且必须已被本机 workspace 验证过；这里只做形状校验（版本、
/// 相对路径）与写入。外键指向的本机登记不存在时由调用方按 workspace 语义映射。
pub(super) async fn insert_binding_row(
    connection: &mut SqliteConnection,
    thread_id: &str,
    binding: &SessionBinding,
) -> Result<()> {
    super::workspace::validate_relative(&binding.cwd_relative_to_workspace)?;
    let version = i64::from(binding.schema_version);
    sqlx::query(
        "INSERT INTO session_bindings (thread_id, schema_version, project_id, workspace_id, relative_cwd)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(thread_id)
    .bind(version)
    .bind(binding.project_id.to_string())
    .bind(binding.workspace_id.to_string())
    .bind(super::discovery::path_text(&binding.cwd_relative_to_workspace)?)
    .execute(&mut *connection)
    .await?;
    Ok(())
}

// ─── 行删除原语 ───────────────────────────────────────────────────────────────

/// 删除 `threads` 行之前必须显式清理的子表：`(子表名, 语句)`。
///
/// **为什么两端都显式删**：远端执行器提供不了级联——远端 schema 不声明任何
/// `REFERENCES`，`PRAGMA foreign_keys` 默认读数为 0、且是跨连接共享的可变状态，远端
/// 也没有 `pragma_foreign_key_check` 等价物（传输面实测结论见母 issue
/// `spec/history/2026-09.md` 2026-09-26 session-store 条目；原文 §9.28 的例外 2/3/4 见 Git 历史）。
/// 一份删除逻辑要跑在两种执行器上，唯一能共用的表达就是显式
/// 删除，因此本机侧也按同一份语句、同一顺序（先子后父）删。
///
/// **为什么本机的 `REFERENCES threads(id) ON DELETE CASCADE` 声明保留**：删外键要重建
/// 表，对用户既有的实盘库是不必要的风险，而收益只是省下几条 DELETE。所以声明留着，
/// 但它从此退化成**空操作式的安全网**——本机删除路径自己已经删过子行，级联再执行时无
/// 行可删；级联是兜底，**不是承重机制**。承重的是这里的语句。
///
/// **表名不是注释，是比对键**：`thread_child_delete_tests` 从运行库的真实 schema 派生
/// 「哪些表的行会随 `threads` 行消失」，并与本清单交叉核对。新增一张
/// `REFERENCES threads(...)` 的子表会让那个测试失败，直到这里的语句补上。
///
/// 移除条件：两端不再依赖显式删除（远端能提供等价的级联语义，或出现统一的删除抽象）
/// 之前，不要删除清单里的任何一项。
pub(super) const THREAD_CHILD_DELETES: &[(&str, &str)] = canonical::THREAD_CHILD_DELETES;

/// 删除 `threads` 行本身的语句；只在 [`delete_thread_child_rows`] 之后执行（先子后父，
/// 与远端 `session_lifecycle` 的 messages → session_bindings → threads 顺序一致）。
pub(super) const DELETE_THREAD_SQL: &str = canonical::DELETE_THREAD_ROW_SQL;

/// 显式删除一条 `threads` 行的全部子行（[`THREAD_CHILD_DELETES`] 逐条执行）。
///
/// 调用方负责随后用 [`DELETE_THREAD_SQL`] 删父行，并与本调用处于同一事务。返回
/// `sqlx::Error` 而不是 `anyhow::Error`：调用点按 [`super::failure::map_sqlx`] 分类
/// 失败（写失败 / 只读 / 忙），不把数据库故障降级成不透明错误。
pub(super) async fn delete_thread_child_rows(
    connection: &mut SqliteConnection,
    thread_id: &str,
) -> std::result::Result<(), sqlx::Error> {
    for (_, statement) in THREAD_CHILD_DELETES {
        sqlx::query(*statement)
            .bind(thread_id)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}
