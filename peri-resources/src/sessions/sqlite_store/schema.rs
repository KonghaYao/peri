//! 单库 schema 升级：保留历史与会话身份，事务内清理退役状态。

use super::database::SqliteSessionDatabase;
#[cfg(test)]
use super::SqliteThreadStore;
use crate::sessions::canonical;
use crate::sessions::schema_shape;
use anyhow::Result;
use peri_acp_types::workspace::WorkspaceError;
use sqlx::{AssertSqlSafe, Connection, SqliteConnection};
use std::collections::HashSet;

/// 本机与远端使用同一会话 schema 版本。
pub(in crate::sessions) use crate::sessions::canonical::CURRENT_SCHEMA_VERSION;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SchemaState {
    /// 没有表：新库。
    Empty,
    /// 旧版未设置 `user_version`：按必需列判定可升级。
    Legacy,
    Version2,
    Version3,
    Version4,
    Version5,
    Version6,
    Version7,
    Version8,
    Version9,
    Version10,
    /// `user_version` 是上一代当前形状（11|12|13）：12 已补齐 inherited 派生索引表但还
    /// 没有 `thread_mq`；13 有 `thread_mq`，只缺 `threads` 的两列工作快照。写打开做一次
    /// 增量补齐，不搬运任何会话事实。
    PreviousCurrent,
    /// `user_version` 已是上一代版本，但形状还是压缩前的登记形状（v4 内部用过）。
    /// 名字取形状而不是版本号：它与 [`PreviousCurrent`](Self::PreviousCurrent) 的差别是
    /// **形状**，不是代数。
    RegistrationShape,
    /// 当前形状。
    Current,
}

impl SchemaState {
    fn needs_registration_rebuild(self) -> bool {
        matches!(
            self,
            Self::Version2 | Self::Version3 | Self::Version4 | Self::Version5
        )
    }
}

/// 旧版未设置 user_version；校验本模块所需基础表，保留同库的其他业务表。
pub(super) async fn inspect(connection: &mut SqliteConnection) -> Result<SchemaState> {
    let (version,): (i64,) = sqlx::query_as("PRAGMA user_version")
        .fetch_one(&mut *connection)
        .await?;
    match version {
        v if v == CURRENT_SCHEMA_VERSION => return current_shape(connection).await,
        // 11..=14 都是可升级来源（11 也可能是压缩前的登记形状）：先交给形状判定，再按判定
        // 结果走对应的幂等升级或搬运。v12 已从常量链移出，这里必须显式列出——否则它会落到
        // `other` 分支被当成「不支持的版本」，把可升级的库关在门外。
        canonical::LEGACY_REGISTRATION_SCHEMA_VERSION
        | 12
        | canonical::LEGACY_CURRENT_SCHEMA_VERSION
        | canonical::PREVIOUS_CURRENT_SCHEMA_VERSION => {
            return previous_shape(connection).await;
        }
        10 => return Ok(SchemaState::Version10),
        9 => return Ok(SchemaState::Version9),
        8 => return Ok(SchemaState::Version8),
        7 => return Ok(SchemaState::Version7),
        6 => return Ok(SchemaState::Version6),
        5 => return Ok(SchemaState::Version5),
        4 => return Ok(SchemaState::Version4),
        3 => return Ok(SchemaState::Version3),
        2 => return Ok(SchemaState::Version2),
        0 => {}
        // 拒绝时复述实际版本：报错要能回答「为什么不支持」，而不是只给结论。
        // 压缩掉的 15..=19 中间代（v4 内部用过、从未正式发布）也落在这里。
        other => {
            return Err(WorkspaceError::UnsupportedSchemaVersion {
                found: other,
                supported: CURRENT_SCHEMA_VERSION,
            }
            .into());
        }
    }
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut *connection)
    .await?;
    if tables.is_empty() {
        return Ok(SchemaState::Empty);
    }
    // 只要求必需的真实表存在，不限制整库的表集合；VIEW 不能替代可升级的表。
    if !["threads", "messages"]
        .iter()
        .all(|required| tables.iter().any(|(name,)| name == required))
    {
        return Err(WorkspaceError::UnsupportedDatabaseSchema.into());
    }
    for (table, required) in [
        (
            "threads",
            &[
                "id",
                "title",
                "cwd",
                "created_at",
                "updated_at",
                "message_count",
            ][..],
        ),
        (
            "messages",
            &["message_id", "thread_id", "role", "content"][..],
        ),
    ] {
        let actual = column_names(connection, table).await?;
        if !required.iter().all(|column| actual.contains(*column)) {
            return Err(WorkspaceError::UnsupportedDatabaseSchema.into());
        }
    }
    Ok(SchemaState::Legacy)
}

/// 版本已是当前版本时的形状判定（fail-closed）：与 `sessions::schema_shape` 的同一份声明
/// 逐项比对（远端只读探测与远端迁移批次用的是同一份声明）。当前版本只接受当前形状：
/// 版本 14 配上缺少两列工作快照的形状不是合法的直接读写来源，不猜形状，也不把
/// 读不懂当成可用。
async fn current_shape(connection: &mut SqliteConnection) -> Result<SchemaState> {
    let tables =
        schema_shape::read_local_columns(&mut *connection, schema_shape::CURRENT_COLUMNS_SQL)
            .await?;
    match schema_shape::check_current_shape(&tables) {
        Ok(()) => Ok(SchemaState::Current),
        Err(reason) => {
            // 拒绝的原因要能回答「为什么」：错误类型本身不携带形状诊断（协议层只有分类），
            // 因此把逐项漂移写进日志，而不是让它只活在代码里。
            tracing::warn!(
                current = %reason,
                "session database shape is not supported by this build"
            );
            Err(WorkspaceError::UnsupportedDatabaseSchema.into())
        }
    }
}

/// 上一代当前形状（11..=14）的形状判定：先看 v14 冻结声明（11 张表，含两列工作快照），
/// 再依次退让到 v13（`threads` 少两列）、v12（还缺 `thread_mq`），最后看压缩前的登记形状
/// （搬运输入，只可能是 11）。都不是就拒绝——这些版本号只服务这些已知输入。
async fn previous_shape(connection: &mut SqliteConnection) -> Result<SchemaState> {
    let tables =
        schema_shape::read_local_columns(&mut *connection, schema_shape::CURRENT_COLUMNS_SQL)
            .await?;
    let v14_reason = match schema_shape::check_v14_shape(&tables) {
        Ok(()) => return Ok(SchemaState::PreviousCurrent),
        Err(reason) => reason,
    };
    let current_reason = match schema_shape::check_v13_shape(&tables) {
        Ok(()) => return Ok(SchemaState::PreviousCurrent),
        Err(reason) => reason,
    };
    let legacy_reason = match schema_shape::check_v12_shape(&tables) {
        Ok(()) => return Ok(SchemaState::PreviousCurrent),
        Err(reason) => reason,
    };
    let mut tables =
        schema_shape::read_local_columns(&mut *connection, schema_shape::INPUT_COLUMNS_SQL).await?;
    let mut dropped_tables =
        schema_shape::read_local_columns(&mut *connection, schema_shape::DROPPED_COLUMNS_SQL)
            .await?;
    for (table, columns) in dropped_tables.drain() {
        tables.entry(table).or_insert(columns);
    }
    let input_reason = schema_shape::check_input_shape(&tables)
        .and_then(|()| schema_shape::check_dropped_shape(&tables));
    if input_reason.is_ok() {
        return Ok(SchemaState::RegistrationShape);
    }
    tracing::warn!(
        v14 = %v14_reason,
        current = %current_reason,
        legacy = %legacy_reason,
        input = %input_reason.unwrap_err(),
        "session database shape is not supported by this build"
    );
    Err(WorkspaceError::UnsupportedDatabaseSchema.into())
}

/// 升级路径的同名预检：canonical 表名上若已经立着**别的类型**的对象（例如同名 VIEW），
/// `CREATE TABLE IF NOT EXISTS` 会静默跳过，把一个不是 canonical 形状的对象留在表的位置上。
/// 这里显式拒绝，保持迁移的 fail-closed（与 v10 删除旧表前的形状校验同一思路）。
///
/// 只覆盖表名（DDL 里的索引名同样是 `IF NOT EXISTS`，同名索引冲突不在本预检范围内）。
/// 错误文案带上对象名与它的实际类型：报错要能回答「为什么不支持」。
async fn ensure_canonical_names_hold_tables(
    connection: &mut SqliteConnection,
    tables: &[&str],
) -> Result<()> {
    for table in tables {
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT type FROM sqlite_schema WHERE name = ?1")
                .bind(table)
                .fetch_optional(&mut *connection)
                .await?;
        if let Some((actual_type,)) = existing {
            if actual_type != "table" {
                anyhow::bail!("schema upgrade blocked: {table} already exists as a {actual_type}");
            }
        }
    }
    Ok(())
}

async fn column_names(connection: &mut SqliteConnection, table: &str) -> Result<HashSet<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info(?)")
        .bind(table)
        .fetch_all(connection)
        .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

impl SqliteSessionDatabase {
    /// DDL 与版本号在同一事务中提交；不回填历史 SessionBinding。
    pub(super) async fn init_schema(&self) -> Result<()> {
        crate::sessions::machine::initialize().await?;
        let mut connection = self.pool.acquire().await?;
        let state = inspect(&mut connection).await?;
        if state == SchemaState::Current {
            // 形状判定只覆盖表与列，索引不在其中：`CREATE_INDEXES` 会随后续构建演进，
            // 当前形状的库同样要按清单幂等补齐（`IF NOT EXISTS`），否则升级上来的库
            // 永远拿不到新索引。语句文本来自 `canonical` 的静态清单，不含外部输入。
            for statement in canonical::CREATE_INDEXES {
                sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
                    .execute(&mut *connection)
                    .await?;
            }
            sqlx::query(canonical::CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL)
                .execute(&mut *connection)
                .await?;
            sqlx::query(canonical::INSERT_CURRENT_MACHINE_SQL)
                .bind(crate::sessions::machine::current()?)
                .execute(&mut *connection)
                .await?;
            return Ok(());
        }
        if state == SchemaState::Empty {
            return Self::create_current_schema(&mut connection).await;
        }
        if state == SchemaState::PreviousCurrent {
            // 上一代 canonical 形状：补齐到 v14 的增量（`thread_mq` 与两列工作快照），
            // 再跑 v15 的重建清理。没有需要搬运的会话事实。
            //
            // v15 重建要 DROP `projects` / `workspaces`——它们被子表引用，外键打开时
            // `DROP TABLE` 的隐式删除会立即检查引用。`PRAGMA foreign_keys` 在事务内是空操作，
            // 因此按 `migrate_schema` 同一手法在**事务外**关闭，提交前用 `foreign_key_check`
            // 补齐校验，最后恢复连接设置。
            sqlx::query("PRAGMA foreign_keys = OFF")
                .execute(&mut *connection)
                .await?;
            let migrated = Self::upgrade_previous_current(&mut connection).await;
            let restored = sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&mut *connection)
                .await;
            migrated?;
            restored?;
            return Ok(());
        }
        if state == SchemaState::RegistrationShape {
            // 压缩前的登记形状：形状本身就是搬运输入，直接走单条搬运。
            return super::storage_v11_migration::migrate_local_v11(&mut connection).await;
        }
        // ≤10 与旧版库：先补齐到压缩前的形状，再走同一条搬运，两段同一个事务，版本只在
        // 搬运完成后推进。搬运要 DROP 被引用的父表并重建绑定：SQLite 对父表做隐式删除时
        // 会立即检查外键，`defer_foreign_keys` 也挡不住；该 PRAGMA 只在事务外生效，
        // 因此整段使用同一条连接：先关外键，提交前用 foreign_key_check 补齐校验，
        // 最后恢复连接设置。
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await?;
        let migrated = Self::migrate_schema(&mut connection, state).await;
        let restored = sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await;
        migrated?;
        restored?;
        Ok(())
    }

    /// 空库：直接建当前形状并推进版本。没有可搬运的旧事实，也就不经过搬运路径。
    async fn create_current_schema(connection: &mut SqliteConnection) -> Result<()> {
        ensure_canonical_names_hold_tables(connection, canonical::CANONICAL_TABLES).await?;
        let mut tx = connection.begin_with("BEGIN IMMEDIATE").await?;
        // 建表语句来自 `sessions::canonical`：本机新库与远端初始化下发的是同一份清单
        // （逐条执行，两边执行器的语句单元相同），形状不可能各自漂移。
        for statement in canonical::CREATE_TABLES {
            // 语句文本来自 `canonical` 的静态清单，不含外部输入。
            sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
                .execute(&mut *tx)
                .await?;
        }
        for statement in canonical::CREATE_INDEXES {
            sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
                .execute(&mut *tx)
                .await?;
        }
        // 当前 Machine 即使尚无 Session 也应可查询。不能覆盖已有展示名。
        sqlx::query(canonical::INSERT_CURRENT_MACHINE_SQL)
            .bind(crate::sessions::machine::current()?)
            .execute(&mut *tx)
            .await?;
        sqlx::query(AssertSqlSafe(format!(
            "PRAGMA user_version = {CURRENT_SCHEMA_VERSION}"
        )))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 上一代当前形状（版本 11|12|13|14）→ 当前形状：先把形状补齐到 v14（`thread_mq` 与
    /// 两列工作快照），再跑 v15 的三表重建清理与退役表删除。整段一个事务，失败时库保持
    /// 原版本，旧二进制仍可打开。调用方已在**事务外**关闭外键强制。
    ///
    /// v11→v15 依次补齐 `thread_mq` 与两列、再重建三张表，v12 多补一张表、v13 只缺两列、
    /// v14 什么都不用补——四者共用同一条路径，避免按代际各写一份。
    async fn upgrade_previous_current(connection: &mut SqliteConnection) -> Result<()> {
        ensure_canonical_names_hold_tables(connection, &[canonical::THREAD_MQ_TABLE]).await?;
        let mut tx = connection.begin_with("BEGIN IMMEDIATE").await?;
        // `thread_mq` 与它的排序索引：12→13 的唯一形状增量；对 13/14 的库是同一份幂等 DDL。
        sqlx::query(canonical::CREATE_THREAD_MQ_TABLE_SQL)
            .execute(&mut *tx)
            .await?;
        // 13→14 的 shape 增量：旧行取默认值（`idle` / NULL），旧 `agent_status` 不参与。
        let actual = column_names(&mut tx, "threads").await?;
        for (name, definition) in canonical::THREADS_V14_COLUMNS {
            if !actual.contains(*name) {
                // 标识符和列定义来自 `canonical` 的静态清单，未包含外部输入。
                sqlx::query(AssertSqlSafe(format!(
                    "ALTER TABLE threads ADD COLUMN {name} {definition}"
                )))
                .execute(&mut *tx)
                .await?;
            }
        }
        sqlx::query(canonical::INSERT_CURRENT_MACHINE_SQL)
            .bind(crate::sessions::machine::current()?)
            .execute(&mut *tx)
            .await?;
        // 14→15：三表重建 + 退役表删除 + 索引重放 + 版本推进，全部在本事务内。
        migrate_v15_cleanup(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// 旧版（`Legacy`、2..10）：在本事务内补齐到压缩前的形状（建表、补列、键放宽、
    /// 退役对象清理），再执行单条搬运。调用方已关闭外键强制并在提交后恢复。
    /// 失败回滚后库保持原版本，可由上一版二进制继续打开。
    async fn migrate_schema(connection: &mut SqliteConnection, state: SchemaState) -> Result<()> {
        let mut tx = connection.begin_with("BEGIN IMMEDIATE").await?;
        let removals = super::schema_cleanup::removal_plan(&mut tx).await?;
        if state == SchemaState::Version2 {
            // v2's unused revision column is NOT NULL without a default. Remove
            // it before current writers stop supplying it; all remaining data stays intact.
            sqlx::query("ALTER TABLE session_bindings DROP COLUMN revision")
                .execute(&mut *tx)
                .await?;
        }
        {
            ensure_canonical_names_hold_tables(&mut tx, canonical::V10_CANONICAL_TABLES).await?;
            // 标识符与列定义均来自 `canonical` 的静态清单，不含外部输入。
            for statement in canonical::V10_CREATE_TABLES {
                sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
                    .execute(&mut *tx)
                    .await?;
            }
            // 新库与逐步升级的旧库使用同一列定义，已有列及其值保持原样。
            for (table, columns) in [
                (
                    "threads",
                    &[
                        ("parent_thread_id", "TEXT"),
                        ("snapshot_at_message_id", "TEXT"),
                        ("hidden", "BOOLEAN NOT NULL DEFAULT 0"),
                        ("cancel_policy", "TEXT NOT NULL DEFAULT 'cascade'"),
                        ("config", "TEXT"),
                        ("frozen_context", "TEXT"),
                        ("inherited_context", "TEXT"),
                        ("agent_status", "TEXT NOT NULL DEFAULT 'active'"),
                    ][..],
                ),
                (
                    "messages",
                    &[
                        ("truncated", "BOOLEAN NOT NULL DEFAULT 0"),
                        ("excluded", "BOOLEAN NOT NULL DEFAULT 0"),
                        ("projection", "TEXT"),
                    ][..],
                ),
            ] {
                let actual = column_names(&mut tx, table).await?;
                for (name, definition) in columns {
                    if !actual.contains(*name) {
                        // 标识符和列定义均来自上方静态 schema，未包含外部输入。
                        sqlx::query(AssertSqlSafe(format!(
                            "ALTER TABLE {table} ADD COLUMN {name} {definition}"
                        )))
                        .execute(&mut *tx)
                        .await?;
                    }
                }
            }
            // 索引在建表与补列之后：`idx_threads_updated` 引用后补的列。
            for statement in canonical::V10_CREATE_INDEXES {
                sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
                    .execute(&mut *tx)
                    .await?;
            }
        }
        if matches!(state, SchemaState::Version2 | SchemaState::Version3) {
            migrate_identity_values(&mut tx).await?;
        }
        // 2/3 直升也必须完成登记键迁移；5 既可能已放宽，也可能被旧 writer
        // 漏迁移后误标。统一重建一次，保留健康 5 已有的所有组合登记。
        if state.needs_registration_rebuild() {
            relax_registration_keys(&mut tx).await?;
            // 本次迁移关闭了外键强制，提交前显式补齐引用校验。
            let violations: Vec<(String, i64, String, i64)> =
                sqlx::query_as("PRAGMA foreign_key_check")
                    .fetch_all(&mut *tx)
                    .await?;
            if !violations.is_empty() {
                return Err(WorkspaceError::DiscoveryError(
                    "registration rebuild broke references".into(),
                )
                .into());
            }
        }
        // v10 回退：删除 v7..v9 写下的本机远程痕迹（本机登记、未决锚点、远端操作日志、
        // 按 store 分区的执行域）。对没有这些表的库是幂等的。
        drop_remote_local_state(&mut tx).await?;
        for statement in removals {
            sqlx::query(statement).execute(&mut *tx).await?;
        }
        sqlx::query(AssertSqlSafe(
            canonical::V10_CREATE_OAUTH_CREDENTIALS_TABLE_SQL,
        ))
        .execute(&mut *tx)
        .await?;
        sqlx::query(AssertSqlSafe(
            canonical::CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL,
        ))
        .execute(&mut *tx)
        .await?;
        // 单条搬运：补齐环境事实 → 重建归属与绑定 → 删掉压缩前独有的对象 → 推进版本。
        // 与上面的形状补齐同一个事务：不能先留下已推进版本而形状未收敛的半升级库。
        super::storage_v11_migration::migrate_shape(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

/// v15 清理本体：把 `projects` / `workspaces` / `session_bindings` 重建为 v15 形状，
/// 删除两张 inherited 派生索引表，重放 canonical 索引集并推进 `user_version`。
///
/// 调用方必须已关闭外键强制（`PRAGMA foreign_keys` 在事务内是空操作），且在同一事务内；
/// 提交前用 `PRAGMA foreign_key_check` 补齐校验，失败即整段回滚、库保持原版本。
///
/// 顺序由外键方向决定：`projects` 被 `workspaces.project_id` 与 `session_bindings.project_id`
/// 引用，因此先合并重复 locator 并改绑引用，再重建父表，最后重建绑定表。
pub(super) async fn migrate_v15_cleanup(connection: &mut SqliteConnection) -> Result<()> {
    // 1. 数据前置审计：引用完整性必须先成立，否则合并与重建会把错误的归属固化下来。
    require_referential_integrity(
        connection,
        "schema v15 refuses a database with broken references",
    )
    .await?;
    // 2. 重复 locator 合并：保留 rowid 最早的项目行，先改绑引用、再删重复行。
    merge_duplicate_project_locators(connection).await?;
    // 3. 三表重建。每张表只重建一次：`session_bindings` 的删列与最小执行根投影共用同一份
    //    模板，绝不先建后改。
    let projects_before = count_rows(connection, "projects").await?;
    rebuild_projects(connection).await?;
    if count_rows(connection, "projects").await? > projects_before {
        return Err(WorkspaceError::DiscoveryError(
            "schema v15 project rebuild duplicated rows".into(),
        )
        .into());
    }
    let workspaces_before = count_rows(connection, "workspaces").await?;
    rebuild_workspaces(connection).await?;
    if count_rows(connection, "workspaces").await? != workspaces_before {
        return Err(WorkspaceError::DiscoveryError(
            "schema v15 workspace rebuild changed the row count".into(),
        )
        .into());
    }
    let bindings_before = count_rows(connection, "session_bindings").await?;
    rebuild_bindings(connection).await?;
    if count_rows(connection, "session_bindings").await? != bindings_before {
        return Err(WorkspaceError::DiscoveryError(
            "schema v15 binding rebuild changed the row count".into(),
        )
        .into());
    }
    // 4. 退役的派生索引表（payload 先于 snapshot）。`IF EXISTS` 让从未建过它们的库幂等。
    for statement in canonical::DROP_INHERITED_INDEX_TABLES {
        // 语句文本来自 `canonical` 的静态清单，不含外部输入。
        sqlx::query(AssertSqlSafe((*statement).to_owned()))
            .execute(&mut *connection)
            .await?;
    }
    // 5. 索引重放：重建的三张表随 `DROP TABLE` 丢掉了自己的索引。
    for statement in canonical::CREATE_INDEXES {
        sqlx::raw_sql(AssertSqlSafe((*statement).to_owned()))
            .execute(&mut *connection)
            .await?;
    }
    // 6. 外键补齐校验与版本推进。
    require_referential_integrity(connection, "schema v15 rebuild broke references").await?;
    sqlx::query(AssertSqlSafe(format!(
        "PRAGMA user_version = {CURRENT_SCHEMA_VERSION}"
    )))
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// 引用完整性补齐校验：任何违反都拒绝迁移，并报出第一处 `(表, rowid)`。
///
/// 只报标识，不报内容——诊断要能回答「哪里坏了」，但不能把会话数据写进错误文本。
async fn require_referential_integrity(
    connection: &mut SqliteConnection,
    context: &str,
) -> Result<()> {
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&mut *connection)
        .await?;
    if let Some((table, rowid, parent, _)) = violations.first() {
        return Err(WorkspaceError::DiscoveryError(format!(
            "{context}: {table} row {rowid} -> {parent}"
        ))
        .into());
    }
    Ok(())
}

/// 同 locator 多行时保留 rowid 最小的项目行，其余项目行的引用先改绑、后删除。
///
/// 读法与 `observation_project` 的 `ORDER BY rowid ASC LIMIT 1` 一致：项目身份就是
/// locator，最早登记的那一行是它的权威记录。改绑失败会让整段迁移回滚，不会留下
/// 「删了行但引用悬空」的中间态。
async fn merge_duplicate_project_locators(connection: &mut SqliteConnection) -> Result<()> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, locator FROM projects ORDER BY rowid ASC")
            .fetch_all(&mut *connection)
            .await?;
    let mut keep: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut merges: Vec<(String, String)> = Vec::new();
    for (id, locator) in rows {
        match keep.get(&locator) {
            Some(keep_id) => merges.push((id, keep_id.clone())),
            None => {
                keep.insert(locator, id);
            }
        }
    }
    for (drop_id, keep_id) in merges {
        sqlx::query("UPDATE workspaces SET project_id = ?1 WHERE project_id = ?2")
            .bind(&keep_id)
            .bind(&drop_id)
            .execute(&mut *connection)
            .await?;
        sqlx::query("UPDATE session_bindings SET project_id = ?1 WHERE project_id = ?2")
            .bind(&keep_id)
            .bind(&drop_id)
            .execute(&mut *connection)
            .await?;
        sqlx::query("DELETE FROM projects WHERE id = ?1")
            .bind(&drop_id)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

async fn count_rows(connection: &mut SqliteConnection, table: &str) -> Result<i64> {
    // 表名来自本模块的静态清单，不含外部输入。
    let (count,): (i64,) = sqlx::query_as(AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(&mut *connection)
        .await?;
    Ok(count)
}

/// `projects` → v15：建暂存 → 搬入 → 删旧 → 建新 → 搬回 → 删暂存。
async fn rebuild_projects(connection: &mut SqliteConnection) -> Result<()> {
    for statement in [
        canonical::CREATE_REBUILD_PROJECTS_TABLE_SQL,
        canonical::INSERT_PROJECTS_INTO_REBUILD_SQL,
        "DROP TABLE projects",
        canonical::CREATE_PROJECTS_TABLE_SQL,
        canonical::INSERT_REBUILD_INTO_PROJECTS_SQL,
        "DROP TABLE projects_v15",
    ] {
        // 语句文本来自 `canonical` 的静态清单，不含外部输入。
        sqlx::query(AssertSqlSafe(statement.to_owned()))
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// `workspaces` → v15：重建链，`identity` 列不搬。
async fn rebuild_workspaces(connection: &mut SqliteConnection) -> Result<()> {
    for statement in [
        canonical::CREATE_REBUILD_WORKSPACES_TABLE_SQL,
        canonical::INSERT_WORKSPACES_INTO_REBUILD_SQL,
        "DROP TABLE workspaces",
        canonical::CREATE_WORKSPACES_TABLE_SQL,
        canonical::INSERT_REBUILD_INTO_WORKSPACES_SQL,
        "DROP TABLE workspaces_v15",
    ] {
        // 语句文本来自 `canonical` 的静态清单，不含外部输入。
        sqlx::query(AssertSqlSafe(statement.to_owned()))
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// `session_bindings` → v15：重建链，`evidence_origin` 不搬，`discovery_snapshot` 逐行投影
/// 为最小执行根（native 只有 `root`，virtual 另带 `kind`）。
///
/// 投影必须在 Rust 侧完成：远端执行器是否提供 json1 未实测，而两端要求同一份搬运语义。
/// 解析失败的行**拒绝整段升级**——静默降级成 `NULL` 会让 admission 从「有证据」变成
/// 「无证据」，把可执行会话降级为只读历史。
async fn rebuild_bindings(connection: &mut SqliteConnection) -> Result<()> {
    let rows: Vec<(String, i64, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT thread_id, schema_version, project_id, workspace_id, relative_cwd, discovery_snapshot FROM session_bindings",
    )
    .fetch_all(&mut *connection)
    .await?;
    let mut projected = Vec::with_capacity(rows.len());
    for (thread_id, schema_version, project_id, workspace_id, relative_cwd, snapshot) in rows {
        let snapshot = match snapshot.as_deref() {
            // 旧远端缺证据的行保持 NULL：不伪造创建时的执行根。
            None => None,
            Some(raw) => Some(
                crate::sessions::binding_evidence::project(raw).map_err(|_| {
                    WorkspaceError::DiscoveryError(
                        "schema v15 cannot project a binding creation snapshot".into(),
                    )
                })?,
            ),
        };
        projected.push((
            thread_id,
            schema_version,
            project_id,
            workspace_id,
            relative_cwd,
            snapshot,
        ));
    }
    for statement in [
        canonical::CREATE_REBUILD_BINDINGS_TABLE_SQL,
        "DROP TABLE session_bindings",
        canonical::CREATE_BINDINGS_TABLE_SQL,
    ] {
        // 语句文本来自 `canonical` 的静态清单，不含外部输入。
        sqlx::query(AssertSqlSafe(statement.to_owned()))
            .execute(&mut *connection)
            .await?;
    }
    for (thread_id, schema_version, project_id, workspace_id, relative_cwd, snapshot) in projected {
        sqlx::query(
            "INSERT INTO session_bindings_v15 (thread_id, schema_version, project_id, workspace_id, relative_cwd, discovery_snapshot)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&thread_id)
        .bind(schema_version)
        .bind(&project_id)
        .bind(&workspace_id)
        .bind(&relative_cwd)
        .bind(&snapshot)
        .execute(&mut *connection)
        .await?;
    }
    sqlx::query(AssertSqlSafe(
        canonical::INSERT_REBUILD_INTO_BINDINGS_SQL.to_owned(),
    ))
    .execute(&mut *connection)
    .await?;
    sqlx::query("DROP TABLE session_bindings_v15")
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// v10 回退：v7..v9 写下的本机远程痕迹一次性删除。
///
/// 用户裁决撤销了「远程存储在本机留有痕迹」的整套能力（只做初始化时的切换，不做交叉
/// 能力），因此这五张表连同它们的语义一起消失，本地表结构回到 remote 工作之前：不加表、
/// 不加列、没有 store 维度。表清单与建表时的列形状一一对应：
///
/// | 表 | 原来承载的事实 |
/// | --- | --- |
/// | `session_store_registrations` | 本机对远端 store 的登记（执行准入依据） |
/// | `session_lifecycle_commitments` | 本机未决写 / 删除墓碑锚点 |
/// | `session_remote_operations` | 远端操作日志（发送前登记与结清） |
/// | `remote_execution_runs` | 按 `(store, root)` 分区的执行代际 |
/// | `remote_lifecycle_commitments` | 按 `(store, thread_id)` 分区的删除锚点 |
///
/// **删除前先校验形状**：表存在但列不符说明它不是本模块建的表（同名的别的业务表），
/// 此时**不删**并拒绝升级（fail-closed，整个事务回滚），而不是把不认识的数据丢掉。
/// 表不存在时跳过，因此对没有这些表的库是幂等的。
///
/// 不触碰 `threads` / `messages` / `session_bindings` / `workspaces` / `projects`。
async fn drop_remote_local_state(connection: &mut SqliteConnection) -> Result<()> {
    for (table, columns) in DROPPED_LOCAL_TABLES {
        if !table_exists(connection, table).await? {
            continue;
        }
        require_columns(connection, table, columns).await?;
        // 标识符来自下方静态清单，未包含外部输入。
        sqlx::query(AssertSqlSafe(format!("DROP TABLE {table}")))
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// v10 删除的本机表及其建表时的列形状。
///
/// 清单只服务一次回退迁移：v10 跑完这些表不复存在，也没有任何写入路径再引用它们，
/// 因此常量本身可以在下一个 schema 版本里一并删除。
const DROPPED_LOCAL_TABLES: &[(&str, &[&str])] = &[
    (
        "session_store_registrations",
        &[
            "store_id",
            "engine",
            "locator_digest",
            "installation_id",
            "created_at",
        ],
    ),
    (
        "session_lifecycle_commitments",
        &[
            "thread_id",
            "root_id",
            "kind",
            "state",
            "generation",
            "operation_id",
            "detail",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "session_remote_operations",
        &[
            "operation_id",
            "store_id",
            "thread_id",
            "root_id",
            "behavior",
            "digest",
            "state",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "remote_execution_runs",
        &["store_id", "root_id", "generation", "clean"],
    ),
    (
        "remote_lifecycle_commitments",
        &[
            "store_id",
            "thread_id",
            "kind",
            "state",
            "created_at",
            "updated_at",
        ],
    ),
];

async fn table_exists(connection: &mut SqliteConnection, table: &str) -> Result<bool> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_schema WHERE type = 'table' AND name = ?")
            .bind(table)
            .fetch_optional(&mut *connection)
            .await?;
    Ok(row.is_some())
}

async fn require_columns(
    connection: &mut SqliteConnection,
    table: &str,
    required: &[&str],
) -> Result<()> {
    let actual = column_names(connection, table).await?;
    if !required.iter().all(|column| actual.contains(*column)) {
        return Err(WorkspaceError::UnsupportedDatabaseSchema.into());
    }
    Ok(())
}

/// schema 5：登记键从单列唯一放宽为 (定位路径, 文件对象证据) 组合。
///
/// 目录被替换（同路径的新文件对象）或换位（同一对象的新路径）是正常演进：它们
/// 应当得到新的项目与工作区登记，而不是被单列唯一约束挡成不可登记。放宽只影响
/// 唯一性判定，不涉及行内容——同一组合仍然唯一，旧登记、旧绑定与执行状态保持原样。
async fn relax_registration_keys(connection: &mut SqliteConnection) -> Result<()> {
    let before = registration_counts(connection).await?;
    // 两个表互为引用，调用方已为本次重建关闭外键强制并在提交前做 foreign_key_check；
    // 复制必须逐列进行，行内容与引用关系都保持原样。
    sqlx::raw_sql(
        "CREATE TABLE projects_relaxed (
            id TEXT PRIMARY KEY, locator TEXT NOT NULL, object_identity TEXT NOT NULL,
            UNIQUE(locator, object_identity)
        );
        INSERT INTO projects_relaxed (id, locator, object_identity)
            SELECT id, locator, object_identity FROM projects;
        DROP TABLE projects;
        ALTER TABLE projects_relaxed RENAME TO projects;
        CREATE TABLE workspaces_relaxed (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
            root TEXT NOT NULL, root_identity TEXT NOT NULL, discovery TEXT NOT NULL,
            UNIQUE(root, root_identity), UNIQUE(id, project_id)
        );
        INSERT INTO workspaces_relaxed (id, project_id, root, root_identity, discovery)
            SELECT id, project_id, root, root_identity, discovery FROM workspaces;
        DROP TABLE workspaces;
        ALTER TABLE workspaces_relaxed RENAME TO workspaces;",
    )
    .execute(&mut *connection)
    .await?;
    if registration_counts(connection).await? != before {
        return Err(WorkspaceError::DiscoveryError(
            "registration rows changed during schema migration".into(),
        )
        .into());
    }
    Ok(())
}

async fn registration_counts(connection: &mut SqliteConnection) -> Result<(i64, i64)> {
    let (projects,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM projects")
        .fetch_one(&mut *connection)
        .await?;
    let (workspaces,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM workspaces")
        .fetch_one(&mut *connection)
        .await?;
    Ok((projects, workspaces))
}

async fn migrate_identity_values(connection: &mut SqliteConnection) -> Result<()> {
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name IN ('projects', 'workspaces')",
    )
    .fetch_all(&mut *connection)
    .await?;
    if tables.len() != 2 {
        return Err(WorkspaceError::UnsupportedDatabaseSchema.into());
    }

    let projects: Vec<(String, String)> =
        sqlx::query_as("SELECT id, object_identity FROM projects ORDER BY id")
            .fetch_all(&mut *connection)
            .await?;
    let mut normalized_projects = Vec::with_capacity(projects.len());
    let mut project_identities = HashSet::new();
    for (id, identity) in projects {
        let identity = normalize_legacy_identity(&identity)?;
        if !project_identities.insert(identity.clone()) {
            return Err(WorkspaceError::DiscoveryError(
                "object identity collision during schema migration".into(),
            )
            .into());
        }
        normalized_projects.push((id, identity));
    }

    let workspaces: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, root_identity, discovery FROM workspaces ORDER BY id")
            .fetch_all(&mut *connection)
            .await?;
    let mut normalized_workspaces = Vec::with_capacity(workspaces.len());
    let mut workspace_identities = HashSet::new();
    for (id, root_identity, discovery) in workspaces {
        let identity = normalize_legacy_identity(&root_identity)?;
        if !workspace_identities.insert(identity.clone()) {
            return Err(WorkspaceError::DiscoveryError(
                "workspace identity collision during schema migration".into(),
            )
            .into());
        }
        normalized_workspaces.push((id, identity, discovery));
    }
    // Validate every row and all collisions before touching unique columns.
    for (id, identity) in normalized_projects {
        sqlx::query("UPDATE projects SET object_identity = ? WHERE id = ?")
            .bind(identity)
            .bind(id)
            .execute(&mut *connection)
            .await?;
    }
    for (id, identity, discovery) in normalized_workspaces {
        sqlx::query("UPDATE workspaces SET root_identity = ?, discovery = ? WHERE id = ?")
            .bind(identity)
            .bind(discovery)
            .bind(id)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// 旧登记形状（v2/v3）的文件对象身份规范化：只接受 `device`/`inode` 与两个已退役的
/// 创建时间字段，输出 `{"device":..,"inode":..}`。
///
/// 这条路径服务的是**压缩前**的登记形状（`projects`/`workspaces` 里的
/// `object_identity`/`root_identity`），它们在 v11 搬运时整表重建、迁移终点不再有这些列。
/// 规范化因此留在本机迁移内，不跟随已退役的 `discovery::ObjectIdentity`。
fn normalize_legacy_identity(raw: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let object = value
        .as_object()
        .ok_or_else(|| WorkspaceError::DiscoveryError("invalid object identity".into()))?;
    if !object.keys().all(|key| {
        matches!(
            key.as_str(),
            "device" | "inode" | "birth_seconds" | "birth_nanos"
        )
    }) {
        return Err(WorkspaceError::DiscoveryError("unknown object identity field".into()).into());
    }
    let device = object
        .get("device")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| WorkspaceError::DiscoveryError("invalid object identity device".into()))?;
    let inode = object
        .get("inode")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| WorkspaceError::DiscoveryError("invalid object identity inode".into()))?;
    Ok(serde_json::json!({ "device": device, "inode": inode }).to_string())
}

#[cfg(test)]
#[path = "schema_test.rs"]
mod tests;
