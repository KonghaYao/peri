//! 压缩后的远端单条迁移：压缩前的存储契约（`v2`，版本 10|11）→ 当前契约（`v5`，版本 11）。
//!
//! 远端与本地共用同一份搬运**规则**（`sessions::storage_v2_plan`）与同一份目标 DDL
//! （`sessions::canonical`），差别只在执行器：远端把整段迁移作为**一次受管批次**下发。
//! 这里因此不重写迁移语义，只补两样远端特有的东西：
//!
//! - **守卫**：形状快照、逐对象定义、逐行会话与绑定核对插在批次最前，任何一条不成立都让
//!   批次以唯一键冲突失败——读到形状与提交时形状必须一致，迁移不作用在漂移过的库上。
//!   本机在事务内读回判定，远端没有事务内读回，只能把同一条规则换成本批次的载体
//!   （`INSERT ... SELECT 0 WHERE (<核对>) <> 0`）。另有一条**结果守卫**
//!   （[`GUARD_MESSAGES_COUNT_SQL`]）例外地放在重建链之后：它核对的不是输入形状，而是
//!   「搬完之后历史还在不在」，前置守卫拦不住这个失败模式。
//! - **版本与契约推进**：作为批次最后一条，把 `(v2, 10|11)` 一次推进到
//!   [`schema::STORE_CONTRACT`] 的当前代数——形状变了，代数跟着变，旧构建凭契约标签拒绝。
//!
//! 读分两批：形状一批（对象定义与列清单，任何库都读得动），有会话表时再读一批搬运输入。
//! 空库（v2 契约但还没有 canonical 表）是同一路径的退化：没有可搬运的行，直接建当前形状。
//! 两批之间没有事务，漂移交给批次里的守卫拦下。
//!
//! 批次的收尾顺序是固定的：**整份 canonical 索引集**（`CREATE_INDEXES`，`IF NOT EXISTS`
//! 幂等）一次性重放，然后才是版本与契约推进——两个被重建的表（`threads` /
//! `session_bindings`）随 `DROP TABLE` 丢掉了自己的索引，逐表补清单会漏掉「表被重建而索引
//! 清单没跟上」的漂移；末条是版本推进，`apply_schema_upgrade` 按「末条影响 1 行」确认升级
//! 真的落了地（见 `RemoteStore::apply_schema_upgrade`）。
//!
//! 打开路径上的形状探测（`schema::shape_probe`）与这里**各自**读形状：探测只决定这次打开
//! 认不认识这个库，迁移则按自己读到的事实构造并守护批次，两者不互相授权。

use std::collections::{BTreeSet, HashMap};

use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::workspace::{WorkspaceId, WorkspacePathSource};
use turso_serverless::Value;

use super::mutation::{incomplete_reply, RemoteStore};
use super::schema::{self, StoreSnapshot};
use super::sql::{int_at, text_at, StatementSpec};
use crate::sessions::canonical;
use crate::sessions::schema_cleanup::{self, ColumnShape, SchemaObject};
use crate::sessions::schema_shape;
use crate::sessions::storage_v2_plan::{
    derive_remote_root, machine_identity, path_source_name, plan_binding_rows,
    plan_local_workspaces, plan_workspace_rows, LegacyBindingRow, LegacyRegistration,
    LegacySession,
};

/// 会话行：机器归属可能缺行（旧版打开路径不写环境表）。
const READ_SESSIONS_SQL: &str = "SELECT t.id, t.parent_thread_id, t.cwd, e.machine_id
    FROM threads t LEFT JOIN session_environments e ON e.thread_id = t.id ORDER BY t.id";
/// 环境表整表缺失时（迁移输入允许它缺席）的会话行：机器归属全部按缺行处理（NULL），
/// 由 [`decode_legacy`] 落到兜底机器——读法不因为可选表不在而失败。
const READ_SESSIONS_WITHOUT_ENVIRONMENTS_SQL: &str =
    "SELECT id, parent_thread_id, cwd, NULL FROM threads ORDER BY id";
/// 绑定行：`workspace_id` 指向执行登记，不是归属行。
const READ_BINDINGS_SQL: &str =
    "SELECT thread_id, schema_version, project_id, workspace_id, relative_cwd
    FROM session_bindings ORDER BY thread_id";
/// 登记行：归属证据随行搬到当前形状的归属行。
const READ_REGISTRATIONS_SQL: &str =
    "SELECT id, project_id, root, root_identity, discovery FROM workspaces ORDER BY id";
/// 历史行数：重建链之后的结果守卫（[`GUARD_MESSAGES_COUNT_SQL`]）按它核对搬完之后的历史还在不在。
const MESSAGES_COUNT_SQL: &str = "SELECT COUNT(*) FROM messages";

const GUARD_OBJECT_COUNT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM sqlite_master WHERE substr(lower(name), 1, 7) <> 'sqlite_') <> ?1";
const GUARD_OBJECT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS
    (SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2 AND tbl_name = ?3 AND sql IS ?4)";
const GUARD_META_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS
    (SELECT 1 FROM peri_store_meta WHERE singleton = 0 AND schema_version = ?1 AND store_id = ?2 AND contract = ?3)";
const GUARD_THREAD_COUNT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE (SELECT COUNT(*) FROM threads) <> ?1";
const GUARD_THREAD_ROW_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM threads t LEFT JOIN session_environments e ON e.thread_id = t.id
    WHERE t.id = ?1 AND t.parent_thread_id IS ?2 AND t.cwd = ?3 AND e.machine_id IS ?4)";
/// 环境表整表缺失时的会话行守卫：与 [`READ_SESSIONS_WITHOUT_ENVIRONMENTS_SQL`] 读回的同一事实
/// （机器归属恒为 NULL）。
const GUARD_THREAD_ROW_WITHOUT_ENVIRONMENTS_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM threads t WHERE t.id = ?1 AND t.parent_thread_id IS ?2 AND t.cwd = ?3 AND ?4 IS NULL)";
const GUARD_BINDING_COUNT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM session_bindings) <> ?1";
const GUARD_BINDING_ROW_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM session_bindings
    WHERE thread_id = ?1 AND schema_version = ?2 AND project_id = ?3 AND workspace_id = ?4 AND relative_cwd = ?5)";
/// 登记行的守卫：两批读取之间对 `workspaces` 的并发写入如果不拦，迁移会按旧快照重建这张表
/// 并覆盖并发数据。逐行守卫覆盖登记行的全部事实列（与 `READ_REGISTRATIONS_SQL` 同列）。
///
/// `pub(super)`：这两条与下一条守卫是批次的拒绝面，由 `schema_upgrade_test` 按同一条语句
/// 断言它们仍在批次里、且落在破坏性语句之前。
pub(super) const GUARD_REGISTRATION_COUNT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM workspaces) <> ?1";
pub(super) const GUARD_REGISTRATION_ROW_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM workspaces
    WHERE id = ?1 AND project_id = ?2 AND root = ?3 AND root_identity = ?4 AND discovery = ?5)";
/// 重建前的归属守卫：重建后的 `workspace_id` 是 NOT NULL，无归属的会话行会让重建在服务端
/// 失败并回滚整批——那时行已经搬进过暂存表。守卫插在重建链**之前**把拒绝提前：它之前的
/// `ALTER` / `UPDATE` 由整批回滚覆盖，没有任何行被搬进当前形状。
pub(super) const GUARD_UNASSIGNED_THREADS_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0
    WHERE EXISTS (SELECT 1 FROM threads WHERE workspace_id IS NULL)";
/// 重建链之后的**结果守卫**：搬完之后历史行数必须与搬到的一致。
///
/// 为什么要它：`DROP TABLE threads` 在外键被打开时不是简单删除，而是**隐式删除 + 级联**——
/// `messages.thread_id REFERENCES threads(id) ON DELETE CASCADE` 会跟着清空全部历史，而远端
/// 服务端的 `PRAGMA foreign_keys` 是跨连接共享的可变状态（见 `RemoteStore::force_parent_checks_off`），
/// 归位失效时本机收不到任何信号；远端也没有本机那样的 `PRAGMA foreign_key_check` 兜底。
/// 于是把「历史还在不在」做成批次里的一条断言：行数不符即唯一键冲突，整批回滚，历史原样保留。
///
/// 位置与其余守卫相反（其余都在任何搬动之前）：级联只发生在重建链里，前置核对看不到它。
pub(super) const GUARD_MESSAGES_COUNT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0
    WHERE (SELECT COUNT(*) FROM messages) <> ?1";
const INSERT_MACHINE_SQL: &str =
    "INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, ?2, ?3)";
const INSERT_WORKSPACE_SQL: &str =
    "INSERT INTO workspaces(id, machine_id, path, path_source, project_id, identity, discovery)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
const INSERT_BINDING_SQL: &str = "INSERT INTO session_bindings(thread_id, schema_version, project_id, workspace_id, relative_cwd, discovery_snapshot, evidence_origin)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
const UPDATE_THREAD_WORKSPACE_SQL: &str = "UPDATE threads SET workspace_id = ?2 WHERE id = ?1";
const ADVANCE_VERSION_SQL: &str =
    "UPDATE peri_store_meta SET schema_version = ?1, contract = ?2 WHERE singleton = 0 AND schema_version = ?3 AND store_id = ?4 AND contract = ?5";

fn unsupported() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Unsupported)
}

fn optional_text(row: &[Value], index: usize) -> SessionResourceResult<Option<String>> {
    match row.get(index) {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(value)) => Ok(Some(value.clone())),
        _ => Err(unsupported()),
    }
}

fn optional_string(row: &[Value], index: usize) -> SessionResourceResult<Option<String>> {
    optional_text(row, index)
}

/// `sqlite_master` 行 → 对象定义（守卫逐对象比对时用同一份值）。
fn decode_objects(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<SchemaObject>> {
    rows.iter()
        .map(|row| {
            Ok(SchemaObject {
                kind: text_at(row, 0).ok_or_else(unsupported)?.to_owned(),
                name: text_at(row, 1).ok_or_else(unsupported)?.to_owned(),
                table: text_at(row, 2).ok_or_else(unsupported)?.to_owned(),
                sql: optional_string(row, 3)?,
            })
        })
        .collect()
}

fn decode_columns(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<ColumnShape>> {
    rows.iter()
        .map(|row| {
            Ok(ColumnShape {
                name: text_at(row, 0).ok_or_else(unsupported)?.to_owned(),
                kind: text_at(row, 1).ok_or_else(unsupported)?.to_owned(),
                not_null: int_at(row, 2).ok_or_else(unsupported)?,
                default: optional_string(row, 3)?,
                primary_key: int_at(row, 4).ok_or_else(unsupported)?,
                hidden: int_at(row, 5).ok_or_else(unsupported)?,
            })
        })
        .collect()
}

/// 一次一致读取得到的迁移输入（原始行 + 形状快照）。
pub(super) struct MigrationRead {
    sessions: Vec<Vec<Value>>,
    bindings: Vec<Vec<Value>>,
    registrations: Vec<Vec<Value>>,
    objects: Vec<SchemaObject>,
    /// `threads` 的完整列形状（含类型与默认值）：退役缓存列的判定要它，不是迁移输入判定。
    thread_columns: Vec<ColumnShape>,
    /// 迁移输入各表的列形状（[`schema_shape::INPUT_COLUMNS_SQL`]，与本机迁移同一份读法）。
    input_columns: HashMap<String, Vec<schema_shape::ActualColumn>>,
    /// 搬到之前的 `messages` 行数（结果守卫的基准）。`None` = 输入形状里没有这张表——
    /// 那时形状判定会以「不认识的输入」拒绝整条迁移，守卫本身不会被构造。
    messages_count: Option<i64>,
}

impl MigrationRead {
    /// 迁移输入里是否存在 `session_environments`：它是可选表（正式基线 10 的库可能没有），
    /// 会话行与逐行守卫的读法按它在不在分岔。
    fn has_environments(&self) -> bool {
        self.declares("session_environments")
    }

    /// 第一批读到的形状里是否有这张表（缺表在结果里没有行，与「空形状」不同）。
    fn declares(&self, table: &str) -> bool {
        self.input_columns
            .get(table)
            .is_some_and(|columns| !columns.is_empty())
    }
}

pub(super) async fn upgrade(
    store: &RemoteStore,
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<()> {
    if !matches!(snapshot.schema_version, 10 | 11)
        || snapshot.contract != schema::PREVIOUS_STORE_CONTRACT
    {
        return Err(unsupported());
    }
    let read = read_input(store).await?;
    // 当前 Machine 也要在批次里立起来（与本机迁移同一条语句）。写打开在此之前从不需要
    // 本机身份，所以这里显式初始化一次：拿不到身份就不发批次。
    crate::sessions::machine::initialize()
        .await
        .map_err(|_| unsupported())?;
    let current_machine = crate::sessions::machine::current().map_err(|_| unsupported())?;
    // 批次里有 `DROP TABLE threads`（重建归属列）：服务端的 `foreign_keys` 是跨连接共享的
    // 可变状态，若被别的连接打开，隐式删除会级联清掉 `messages`。写打开与这里一样先归位。
    store.force_parent_checks_off().await?;
    store
        .apply_schema_upgrade(upgrade_plan(snapshot, &read, current_machine)?)
        .await
}

/// 迁移输入的一致读取：形状一批（对象定义与列清单，任何库都读得动），有会话表时再读一批
/// 搬运输入。两批之间没有事务，漂移交给批次里的守卫拦下。
///
/// 生产（[`upgrade`]）与批次构造的离线断言共用这一处读法：批次里的守卫按这里读到的事实
/// 复核，两者因此逐列同源。
pub(super) async fn read_input(store: &RemoteStore) -> SessionResourceResult<MigrationRead> {
    let shape = store
        .read_batch(vec![
            StatementSpec::bare(schema_cleanup::SCHEMA_OBJECTS_SQL),
            StatementSpec::bare(schema_cleanup::THREAD_COLUMNS_SQL),
            StatementSpec::bare(schema_shape::INPUT_COLUMNS_SQL),
        ])
        .await?;
    let mut read = MigrationRead {
        sessions: Vec::new(),
        bindings: Vec::new(),
        registrations: Vec::new(),
        objects: decode_objects(&shape[0])?,
        thread_columns: decode_columns(&shape[1])?,
        input_columns: schema::decode_column_map(&shape[2])?,
        messages_count: None,
    };
    if read.thread_columns.is_empty() {
        return Ok(read);
    }
    let sessions_sql = if read.has_environments() {
        READ_SESSIONS_SQL
    } else {
        READ_SESSIONS_WITHOUT_ENVIRONMENTS_SQL
    };
    let rows = store
        .read_batch(vec![
            StatementSpec::bare(sessions_sql),
            StatementSpec::bare(READ_BINDINGS_SQL),
            StatementSpec::bare(READ_REGISTRATIONS_SQL),
        ])
        .await?;
    read.sessions = rows[0].clone();
    read.bindings = rows[1].clone();
    read.registrations = rows[2].clone();
    // 历史行数只在这张表确实存在时读（与 `has_environments` 同一条规则）：表缺席时形状判定
    // 会以 `Unsupported` 拒绝整条迁移，这里不能把那个拒绝降级成一次「表不存在」的读失败。
    if read.declares("messages") {
        let row = store
            .fetch_row(&StatementSpec::bare(MESSAGES_COUNT_SQL))
            .await?;
        // 「读不到计数」不是「没有历史」：按不完整读上报，绝不让守卫以 0 为基准放行。
        let count = row
            .as_ref()
            .and_then(|values| int_at(values, 0))
            .ok_or_else(|| incomplete_reply("message count read returned no count"))?;
        read.messages_count = Some(count);
    }
    Ok(read)
}

/// 从一次一致快照构造迁移批次（纯函数，离线断言语句顺序与拒绝形状）；
/// `current_machine` 是本次写打开的本机身份行。
pub(super) fn upgrade_plan(
    snapshot: &StoreSnapshot,
    read: &MigrationRead,
    current_machine: &str,
) -> SessionResourceResult<Vec<StatementSpec>> {
    if !matches!(snapshot.schema_version, 10 | 11)
        || snapshot.contract != schema::PREVIOUS_STORE_CONTRACT
    {
        return Err(unsupported());
    }
    let mut statements = vec![StatementSpec::new(
        GUARD_META_SQL,
        vec![
            Value::Integer(snapshot.schema_version),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    )];
    // 空库：没有 canonical 表可搬，直接建当前形状（版本与契约的推进在最后一条）。
    if read.thread_columns.is_empty() {
        if read.objects.iter().any(|object| {
            canonical::V10_CANONICAL_TABLES
                .iter()
                .chain(&["thread_goals"])
                .any(|name| object.name.eq_ignore_ascii_case(name))
        }) {
            return Err(unsupported());
        }
        statements.push(StatementSpec::new(
            GUARD_OBJECT_COUNT_SQL,
            vec![Value::Integer(read.objects.len() as i64)],
        ));
        for object in &read.objects {
            statements.push(StatementSpec::new(GUARD_OBJECT_SQL, object_values(object)));
        }
        statements.extend(
            canonical::CREATE_TABLES
                .iter()
                .chain(canonical::CREATE_INDEXES)
                .map(|sql| StatementSpec::bare(sql)),
        );
        statements.push(current_machine_statement(current_machine));
        statements.push(advance_version(snapshot));
        return Ok(statements);
    }

    // 迁移输入形状：必需列缺失或出现本构建不认识的列都拒绝。搬运会 DROP 并重建这些表，
    // 未知列的值没有去处，「不认识的形状」因此不能静默搬掉（`schema_shape` 是判定来源，
    // 本机迁移用的是同一份声明）。
    //
    // 下面每条判定都把失败原因折成同一个分类（「不认识的输入」），而错误类型不携带形状诊断
    // （协议层只有分类）——所以逐条把原因写进日志：拒绝要能回答「为什么」，与本机迁移把原因
    // 写进错误文本的做法同源。
    schema_shape::check_input_shape(&read.input_columns).map_err(|reason| {
        tracing::warn!(
            reason = %reason,
            "remote store upgrade refuses this migration input shape"
        );
        unsupported()
    })?;
    // 要删除的压缩前对象先判定形状（判定与计划同一个入口）：同名异形的表不删并拒绝整批。
    let dropped = schema_shape::dropped_plan(&read.input_columns).map_err(|reason| {
        tracing::warn!(
            reason = %reason,
            "remote store upgrade refuses to drop this object"
        );
        unsupported()
    })?;
    let removals =
        schema_cleanup::removal_plan(&read.objects, &read.thread_columns).map_err(|reason| {
            tracing::warn!(
                reason = %reason,
                "remote store upgrade refuses these retired objects"
            );
            unsupported()
        })?;
    // 结果守卫的基准：`messages` 缺席时上面那条形状判定已经拒绝，这里再兜一次，不让 `None`
    // 变成一条「跳过守卫」的静默分支。
    let messages_count = read.messages_count.ok_or_else(unsupported)?;
    let (sessions, registrations) = decode_legacy(
        &read.sessions,
        &read.bindings,
        &read.registrations,
        snapshot,
    )?;
    let legacy_bindings = decode_bindings(&read.bindings)?;
    let workspace_plan = plan_local_workspaces(&sessions, &registrations).map_err(|reason| {
        tracing::warn!(
            reason = %reason,
            "remote store upgrade cannot plan workspaces for this legacy input"
        );
        unsupported()
    })?;
    if workspace_plan.session_workspace_ids.len() != sessions.len() {
        return Err(unsupported());
    }
    let mut machines: BTreeSet<String> = workspace_plan
        .workspaces
        .iter()
        .map(|workspace| workspace.machine_id.clone())
        .collect();
    // 当前 Machine 并入集合（与本机迁移同一处语义）：集合同时决定「批次里插哪些机器行」与
    // 「没有会话引用的登记行落在哪台机器上」。库里 threads 为空但有登记行时（会话被删过），
    // 集合为空会让 [`plan_workspace_rows`] 的兜底取不到机器而静默跳过那些行——登记证据
    // （project_id / root_identity / discovery）随之丢失、版本却照常推进。
    machines.insert(current_machine.to_owned());
    let workspace_rows = plan_workspace_rows(&workspace_plan, &sessions, &registrations, &machines)
        .map_err(|reason| {
            tracing::warn!(
                reason = %reason,
                "remote store upgrade cannot plan workspace rows for this legacy input"
            );
            unsupported()
        })?;
    let binding_rows = plan_binding_rows(&legacy_bindings, &workspace_plan, &registrations)
        .map_err(|reason| {
            tracing::warn!(
                reason = %reason,
                "remote store upgrade cannot plan binding rows for this legacy input"
            );
            unsupported()
        })?;

    // 守卫：读到什么形状，提交时就要求什么形状。
    statements.push(StatementSpec::new(
        GUARD_OBJECT_COUNT_SQL,
        vec![Value::Integer(read.objects.len() as i64)],
    ));
    for object in &read.objects {
        statements.push(StatementSpec::new(GUARD_OBJECT_SQL, object_values(object)));
    }
    statements.push(StatementSpec::new(
        GUARD_THREAD_COUNT_SQL,
        vec![Value::Integer(read.sessions.len() as i64)],
    ));
    // 会话行的机器归属读法与环境表是否存在有关，逐行守卫沿用同一读法。
    let thread_row_guard = if read.has_environments() {
        GUARD_THREAD_ROW_SQL
    } else {
        GUARD_THREAD_ROW_WITHOUT_ENVIRONMENTS_SQL
    };
    for row in &read.sessions {
        statements.push(StatementSpec::new(thread_row_guard, row.clone()));
    }
    statements.push(StatementSpec::new(
        GUARD_BINDING_COUNT_SQL,
        vec![Value::Integer(read.bindings.len() as i64)],
    ));
    for row in &read.bindings {
        statements.push(StatementSpec::new(GUARD_BINDING_ROW_SQL, row.clone()));
    }
    // 登记行的守卫：迁移要按读到的快照重建 `workspaces`，两批之间对它的并发写入必须让整批
    // 失败，否则那些行会被旧快照覆盖。
    statements.push(StatementSpec::new(
        GUARD_REGISTRATION_COUNT_SQL,
        vec![Value::Integer(read.registrations.len() as i64)],
    ));
    for row in &read.registrations {
        statements.push(StatementSpec::new(GUARD_REGISTRATION_ROW_SQL, row.clone()));
    }
    statements.extend(removals.into_iter().map(StatementSpec::bare));

    // 旧登记与引用它的绑定表整体换掉：两个 id 空间收敛成一个。
    statements.push(StatementSpec::bare("DROP TABLE session_bindings"));
    statements.push(StatementSpec::bare("DROP TABLE workspaces"));
    statements.push(StatementSpec::bare(canonical::CREATE_MACHINES_TABLE_SQL));
    statements.push(current_machine_statement(current_machine));
    statements.push(StatementSpec::bare(canonical::CREATE_WORKSPACES_TABLE_SQL));
    for machine_id in &machines {
        let (name, kind) = machine_identity(machine_id);
        statements.push(StatementSpec::new(
            INSERT_MACHINE_SQL,
            vec![
                Value::Text(machine_id.clone()),
                Value::Text(name.to_owned()),
                Value::Text(kind.to_owned()),
            ],
        ));
    }
    for row in &workspace_rows {
        let path = row.path.to_str().ok_or_else(unsupported)?;
        statements.push(StatementSpec::new(
            INSERT_WORKSPACE_SQL,
            vec![
                Value::Text(row.id.to_string()),
                Value::Text(row.machine_id.clone()),
                Value::Text(path.to_owned()),
                Value::Text(path_source_name(row.path_source).to_owned()),
                optional_value(&row.project_id),
                optional_value(&row.identity),
                optional_value(&row.discovery),
            ],
        ));
    }
    statements.push(StatementSpec::bare(
        "ALTER TABLE threads ADD COLUMN workspace_id TEXT REFERENCES workspaces(id)",
    ));
    statements.push(StatementSpec::bare(
        "ALTER TABLE threads ADD COLUMN archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))",
    ));
    for session in &sessions {
        let workspace_id = workspace_plan
            .session_workspace_ids
            .get(&session.id)
            .ok_or_else(unsupported)?;
        statements.push(StatementSpec::new(
            UPDATE_THREAD_WORKSPACE_SQL,
            vec![
                Value::Text(session.id.clone()),
                Value::Text(workspace_id.to_string()),
            ],
        ));
    }
    // 归属列落在 **NOT NULL** 上：`ALTER` 只能加可空列，只能重建（与本机迁移同一条路径）。
    // 无归属的会话行如果拖到重建才暴露，失败时已经搬进过暂存表；这条守卫因此插在重建链
    // **之前**（它前面的 `ALTER` / `UPDATE` 由整批回滚覆盖），让拒绝发生在搬动之前。
    statements.push(StatementSpec::bare(GUARD_UNASSIGNED_THREADS_SQL));
    statements.extend(
        [
            canonical::CREATE_REBUILD_THREADS_TABLE_SQL,
            canonical::INSERT_THREADS_INTO_REBUILD_SQL,
            "DROP TABLE threads",
            canonical::CREATE_THREADS_TABLE_SQL,
            canonical::INSERT_REBUILD_INTO_THREADS_SQL,
            "DROP TABLE threads_rebuild",
        ]
        .into_iter()
        .map(StatementSpec::bare),
    );
    // 结果守卫：重建链走完才核对历史行数——`DROP TABLE threads` 在父行检查被打开时会隐式
    // 删除并级联清空 `messages`，前置守卫看不到这一步（见 [`GUARD_MESSAGES_COUNT_SQL`]）。
    statements.push(StatementSpec::new(
        GUARD_MESSAGES_COUNT_SQL,
        vec![Value::Integer(messages_count)],
    ));
    statements.push(StatementSpec::bare(canonical::CREATE_BINDINGS_TABLE_SQL));
    for row in &binding_rows {
        statements.push(StatementSpec::new(
            INSERT_BINDING_SQL,
            vec![
                Value::Text(row.thread_id.clone()),
                Value::Integer(row.schema_version),
                Value::Text(row.project_id.clone()),
                Value::Text(row.workspace_id.to_string()),
                Value::Text(row.relative_cwd.clone()),
                Value::Text(row.discovery_snapshot.clone()),
                Value::Text(row.evidence_origin.to_owned()),
            ],
        ));
    }
    // 压缩前独有的两张表（删除前已按 `schema_shape` 判定形状）：旧凭证只有 machine 作用域，
    // 无法证明属于哪个 Workspace；环境事实与归属行合并之后也不再承载事实。
    statements.extend(dropped.into_iter().map(StatementSpec::bare));
    statements.push(StatementSpec::bare(
        canonical::CREATE_OAUTH_CREDENTIALS_TABLE_SQL,
    ));
    statements.push(StatementSpec::bare(
        canonical::CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL,
    ));
    // 索引重放：`DROP TABLE` 把被重建的表（`threads` / `session_bindings`）上的索引一并删掉，
    // 批次末尾把 canonical 的**整份**索引集一次性重放，升级终点因此天然带齐索引——
    // 按表分组、逐表补索引会漏掉「某张表被重建而那张表的清单没跟上」这种漂移。
    // `IF NOT EXISTS` 让重放幂等（`messages` 的索引本来就在）。
    statements.extend(
        canonical::CREATE_INDEXES
            .iter()
            .map(|sql| StatementSpec::bare(sql)),
    );
    statements.push(advance_version(snapshot));
    Ok(statements)
}

/// 当前 Machine 的登记行：批次与本机迁移下发同一条语句（`canonical` 是唯一来源）。
fn current_machine_statement(current_machine: &str) -> StatementSpec {
    StatementSpec::new(
        canonical::INSERT_CURRENT_MACHINE_SQL,
        vec![Value::Text(current_machine.to_owned())],
    )
}

/// 版本与契约一起推进：形状变了，代数跟着变。
fn advance_version(snapshot: &StoreSnapshot) -> StatementSpec {
    StatementSpec::new(
        ADVANCE_VERSION_SQL,
        vec![
            Value::Integer(schema::REMOTE_SCHEMA_VERSION),
            Value::Text(schema::STORE_CONTRACT.to_owned()),
            Value::Integer(snapshot.schema_version),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    )
}

fn object_values(object: &SchemaObject) -> Vec<Value> {
    vec![
        Value::Text(object.kind.clone()),
        Value::Text(object.name.clone()),
        Value::Text(object.table.clone()),
        object.sql.clone().map(Value::Text).unwrap_or(Value::Null),
    ]
}

fn optional_value(value: &Option<String>) -> Value {
    match value {
        Some(value) => Value::Text(value.clone()),
        None => Value::Null,
    }
}

/// 会话行与绑定行 → 规划输入。
///
/// 会话的机器归属缺行时用 `legacy:<store id>` 兜底（本机以外的旧机器，与未知机器同类）；
/// 绑定记录的根取不到时按保存的 cwd 规划，规划只用于分组，绝不当成执行证据。
fn decode_legacy(
    session_rows: &[Vec<Value>],
    binding_rows: &[Vec<Value>],
    registration_rows: &[Vec<Value>],
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<(Vec<LegacySession>, Vec<LegacyRegistration>)> {
    let registrations = decode_registrations(registration_rows)?;
    let mut bindings: std::collections::HashMap<String, (String, Option<String>)> =
        std::collections::HashMap::new();
    for row in binding_rows {
        let thread_id = text_at(row, 0).ok_or_else(unsupported)?.to_owned();
        // 绑定表的主键是 `thread_id`：读回重复行说明这不是本构建认识的形状（例如同名的
        // VIEW）。后行覆盖前行会让输出比输入少一行、版本却照常推进——显式拒绝。
        let replaced = bindings.insert(
            thread_id,
            (
                text_at(row, 3).ok_or_else(unsupported)?.to_owned(),
                optional_text(row, 4)?,
            ),
        );
        if replaced.is_some() {
            return Err(unsupported());
        }
    }
    let mut sessions = Vec::with_capacity(session_rows.len());
    for row in session_rows {
        let id = text_at(row, 0).ok_or_else(unsupported)?.to_owned();
        let parent_id = optional_text(row, 1)?;
        let cwd = text_at(row, 2).ok_or_else(unsupported)?.to_owned();
        let machine_id = optional_text(row, 3)?
            .unwrap_or_else(|| format!("legacy:{}", snapshot.store_id.as_str()));
        let binding = bindings.get(&id);
        let registration = binding
            .map(|(workspace_id, _)| {
                workspace_id
                    .parse::<WorkspaceId>()
                    .map_err(|_| unsupported())
            })
            .transpose()?;
        // 相对路径与保存的 cwd 推不出同一个根时，绑定记录的根不再可信：
        // 按保存的 cwd 分组（规划用途），绑定本身仍原样搬运并原样校验。
        let derived_root = binding
            .and_then(|(_, relative)| relative.as_deref())
            .and_then(|relative| derive_remote_root(&cwd, relative).ok());
        let plan_registration = if registration.is_some() && derived_root.is_none() {
            None
        } else {
            registration
        };
        sessions.push(LegacySession {
            id,
            parent_id,
            machine_id,
            cwd: cwd.into(),
            execution_workspace_id: plan_registration,
            derived_root,
        });
    }
    // 推不出根、又带着登记 id 的会话必须在别处有同路径的归属：否则规划会静默改绑，
    // 这里显式拒绝（与 `plan_local_workspaces` 的检查互补）。
    for session in &sessions {
        if let (Some(registration_id), Some(derived_root)) = (
            session.execution_workspace_id,
            session.derived_root.as_ref(),
        ) {
            if registrations
                .iter()
                .find(|registration| registration.id == registration_id)
                .is_some_and(|registration| &registration.root != derived_root)
            {
                return Err(unsupported());
            }
        }
    }
    Ok((sessions, registrations))
}

/// 登记行 → 规划输入：观测证据必须描述同一个根，否则这不是本构建认识的登记行。
fn decode_registrations(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<LegacyRegistration>> {
    let mut registrations = Vec::with_capacity(rows.len());
    for row in rows {
        let id: WorkspaceId = text_at(row, 0)
            .ok_or_else(unsupported)?
            .parse()
            .map_err(|_| unsupported())?;
        let project_id = text_at(row, 1).ok_or_else(unsupported)?.to_owned();
        let root = text_at(row, 2).ok_or_else(unsupported)?.to_owned();
        let root_identity = text_at(row, 3).ok_or_else(unsupported)?.to_owned();
        let discovery = text_at(row, 4).ok_or_else(unsupported)?.to_owned();
        let observed: serde_json::Value =
            serde_json::from_str(&discovery).map_err(|_| unsupported())?;
        if observed.get("root").and_then(serde_json::Value::as_str) != Some(root.as_str()) {
            return Err(unsupported());
        }
        let path_source = if observed
            .get("common_dir")
            .is_some_and(|value| !value.is_null())
            || observed
                .get("private_dir")
                .is_some_and(|value| !value.is_null())
        {
            WorkspacePathSource::Discovered
        } else {
            WorkspacePathSource::Unverified
        };
        registrations.push(LegacyRegistration {
            id,
            root: root.into(),
            path_source,
            project_id,
            root_identity,
            discovery,
        });
    }
    Ok(registrations)
}

/// 绑定行 → 搬运输入。
fn decode_bindings(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<LegacyBindingRow>> {
    rows.iter()
        .map(|row| {
            Ok(LegacyBindingRow {
                thread_id: text_at(row, 0).ok_or_else(unsupported)?.to_owned(),
                schema_version: int_at(row, 1).ok_or_else(unsupported)?,
                project_id: text_at(row, 2).ok_or_else(unsupported)?.to_owned(),
                workspace_id: text_at(row, 3)
                    .ok_or_else(unsupported)?
                    .parse()
                    .map_err(|_| unsupported())?,
                relative_cwd: text_at(row, 4).ok_or_else(unsupported)?.to_owned(),
            })
        })
        .collect()
}
