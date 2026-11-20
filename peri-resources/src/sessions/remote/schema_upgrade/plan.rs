//! Atomic migration plans for released store shapes.

use super::super::schema;
use super::super::schema::StoreSnapshot;
use super::super::sql::StatementSpec;
use super::input::MigrationRead;
use super::statements::{
    ADVANCE_VERSION_SQL, GUARD_BINDING_COUNT_SQL, GUARD_BINDING_ROW_SQL,
    GUARD_BINDING_SNAPSHOT_SQL, GUARD_MESSAGES_COUNT_SQL, GUARD_META_SQL, GUARD_OBJECT_COUNT_SQL,
    GUARD_OBJECT_SQL, GUARD_REGISTRATION_COUNT_SQL, GUARD_REGISTRATION_ROW_SQL,
    GUARD_THREAD_COUNT_SQL, GUARD_THREAD_ROW_SQL, GUARD_THREAD_ROW_WITHOUT_ENVIRONMENTS_SQL,
    GUARD_UNASSIGNED_THREADS_SQL, INSERT_BINDING_SQL, INSERT_MACHINE_SQL, INSERT_WORKSPACE_SQL,
    UPDATE_THREAD_WORKSPACE_SQL,
};
use crate::sessions::canonical;
use crate::sessions::schema_cleanup;
use crate::sessions::schema_cleanup::SchemaObject;
use crate::sessions::schema_shape;
use peri_acp_types::session_resources::SessionResourceResult;
use std::collections::BTreeSet;
use std::collections::HashMap;
use turso_serverless::Value;

/// 增量补齐的升级来源：`v7` + 13（只缺两列）、`v6` + 12 与 `v5` + 11
/// （都还缺 `thread_mq`）。
///
/// 三条来源共用同一份幂等批次：建表都是 `CREATE ... IF NOT EXISTS`；两列是 v14 的
/// 唯一 `ALTER`，三条来源都必然缺它们。增量补完后同一批接 v15 清理，终点一次到 v15。
pub(in crate::sessions::remote) fn additive_source(snapshot: &StoreSnapshot) -> bool {
    matches!(
        (snapshot.contract.as_str(), snapshot.schema_version),
        (schema::LEGACY_CURRENT_STORE_CONTRACT, 13)
            | (schema::OLDER_CURRENT_STORE_CONTRACT, 12)
            | (schema::OLDEST_CURRENT_STORE_CONTRACT, 11)
    )
}

/// v15 清理的升级来源：`v8` + 14（v15 之前的当前形状）。
///
/// 它不缺任何列或表，缺的是 v15 的**删除**：三列退役加两张表退役，被唯一键引用的列不能
/// `DROP COLUMN`，只能走重建。因此它不复用 additive 的 `CREATE ... IF NOT EXISTS`，
/// 而是独立的受管批次 [`v15_cleanup_plan`]。
pub(in crate::sessions::remote) fn v15_cleanup_source(snapshot: &StoreSnapshot) -> bool {
    snapshot.contract == schema::PREVIOUS_CURRENT_STORE_CONTRACT && snapshot.schema_version == 14
}

/// 上一代/再上一代当前形状的升级批次：补 `thread_mq` 与两列，末尾接 v15 清理并推进版本。
/// 整批仍是受管批次，守卫与推进语义与搬运路径一致。
///
/// v13/v12/v11 的终点是 v15：批次末尾接的是 v15 清理，因此补完列与表之后直接进入
/// [`v15_cleanup_plan`] 的语句序列（同一批，原子）。
pub(in crate::sessions::remote) fn additive_upgrade_plan(
    snapshot: &StoreSnapshot,
    current_machine: &str,
    binding_snapshots: &[(String, Option<String>)],
    projects: &[(String, String)],
) -> SessionResourceResult<Vec<StatementSpec>> {
    if !additive_source(snapshot) {
        return Err(super::dispatch::unsupported());
    }
    let mut statements = vec![StatementSpec::new(
        GUARD_META_SQL,
        vec![
            Value::Integer(snapshot.schema_version),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    )];
    statements.push(StatementSpec::bare(canonical::CREATE_THREAD_MQ_TABLE_SQL));
    statements.extend(threads_v14_column_statements());
    statements.push(current_machine_statement(current_machine));
    // 补完 v14 的增量之后，同一批接 v15 清理（三表重建 + 删两表 + 索引重放）。
    statements.extend(v15_cleanup_statements(
        current_machine,
        binding_snapshots,
        projects,
    )?);
    statements.push(advance_version(snapshot));
    Ok(statements)
}

/// 从一次一致快照构造迁移批次（纯函数，离线断言语句顺序与拒绝形状）；
/// `current_machine` 是本次写打开的本机身份行。
pub(in crate::sessions::remote) fn upgrade_plan(
    snapshot: &StoreSnapshot,
    read: &MigrationRead,
    current_machine: &str,
) -> SessionResourceResult<Vec<StatementSpec>> {
    if !super::dispatch::legacy_migration_source(snapshot) {
        return Err(super::dispatch::unsupported());
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
            return Err(super::dispatch::unsupported());
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
        super::dispatch::unsupported()
    })?;
    // 要删除的压缩前对象先判定形状（判定与计划同一个入口）：同名异形的表不删并拒绝整批。
    // 退役的两张 inherited 表不在输入读取面里，用同一批里读到的删除面形状补齐。
    let mut dropped_shape = read.input_columns.clone();
    for (table, columns) in &read.dropped_columns {
        dropped_shape
            .entry(table.clone())
            .or_insert_with(|| columns.clone());
    }
    let dropped = schema_shape::dropped_plan(&dropped_shape).map_err(|reason| {
        tracing::warn!(
            reason = %reason,
            "remote store upgrade refuses to drop this object"
        );
        super::dispatch::unsupported()
    })?;
    let removals =
        schema_cleanup::removal_plan(&read.objects, &read.thread_columns).map_err(|reason| {
            tracing::warn!(
                reason = %reason,
                "remote store upgrade refuses these retired objects"
            );
            super::dispatch::unsupported()
        })?;
    // 结果守卫的基准：`messages` 缺席时上面那条形状判定已经拒绝，这里再兜一次，不让 `None`
    // 变成一条「跳过守卫」的静默分支。
    let messages_count = read
        .messages_count
        .ok_or_else(super::dispatch::unsupported)?;
    let (sessions, registrations) = super::input::decode_legacy(
        &read.sessions,
        &read.bindings,
        &read.registrations,
        snapshot,
    )?;
    let legacy_bindings = super::input::decode_bindings(&read.bindings)?;
    let workspace_plan =
        crate::sessions::storage_v2_plan::plan_local_workspaces(&sessions, &registrations)
            .map_err(|reason| {
                tracing::warn!(
                    reason = %reason,
                    "remote store upgrade cannot plan workspaces for this legacy input"
                );
                super::dispatch::unsupported()
            })?;
    if workspace_plan.session_workspace_ids.len() != sessions.len() {
        return Err(super::dispatch::unsupported());
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
    let workspace_rows = crate::sessions::storage_v2_plan::plan_workspace_rows(
        &workspace_plan,
        &sessions,
        &registrations,
        &machines,
    )
    .map_err(|reason| {
        tracing::warn!(
            reason = %reason,
            "remote store upgrade cannot plan workspace rows for this legacy input"
        );
        super::dispatch::unsupported()
    })?;
    let binding_rows = crate::sessions::storage_v2_plan::plan_binding_rows(
        &legacy_bindings,
        &workspace_plan,
        &registrations,
    )
    .map_err(|reason| {
        tracing::warn!(
            reason = %reason,
            "remote store upgrade cannot plan binding rows for this legacy input"
        );
        super::dispatch::unsupported()
    })?;

    // 项目行：v15 的 `projects` 以 `UNIQUE(locator)` 为身份，必须从旧库真实的项目行重建。
    // `read.projects` 按 rowid 升序：同 locator 保留最早一行，其余 id 记进 remap，否则
    // 后面的归属行与绑定行会引用一个不存在的项目。表里缺的 id（旧库没有 projects 表）
    // 退回按归属行路径合成，同 locator 保留 id 最小者。
    let mut project_rows: Vec<(String, String)> = Vec::new();
    let mut locator_owner: HashMap<String, usize> = HashMap::new();
    let mut remap: HashMap<String, String> = HashMap::new();
    for (id, locator) in &read.projects {
        match locator_owner.get(locator) {
            Some(&index) => {
                remap.insert(id.clone(), project_rows[index].0.clone());
            }
            None => {
                locator_owner.insert(locator.clone(), project_rows.len());
                project_rows.push((id.clone(), locator.clone()));
            }
        }
    }
    for row in &workspace_rows {
        let Some(project_id) = row.project_id.clone() else {
            continue;
        };
        if project_rows.iter().any(|(id, _)| *id == project_id) || remap.contains_key(&project_id) {
            continue;
        }
        let locator = row
            .path
            .to_str()
            .ok_or_else(super::dispatch::unsupported)?
            .to_owned();
        match locator_owner.get(&locator) {
            Some(index) if project_rows[*index].0.as_str() <= project_id.as_str() => {
                remap.insert(project_id, project_rows[*index].0.clone());
            }
            Some(index) => {
                let replaced = std::mem::replace(&mut project_rows[*index].0, project_id);
                remap.insert(replaced, project_rows[*index].0.clone());
            }
            None => {
                locator_owner.insert(locator.clone(), project_rows.len());
                project_rows.push((project_id, locator));
            }
        }
    }
    let keep = |project_id: &str| {
        remap
            .get(project_id)
            .cloned()
            .unwrap_or_else(|| project_id.to_owned())
    };

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
    //
    // `projects` 也整表重建：v15 的唯一键收窄为 `UNIQUE(locator)`，真实项目行按
    // `locator` 去重（保留 `read.projects` 的 rowid 最早一行），旧库缺 `projects` 表时
    // 才按归属行路径合成；引用关系由上面的规划保证只指向保留行。顺序：先删引用者
    // （bindings/workspaces），再删父表（projects），最后按 v15 DDL 重建。
    statements.push(StatementSpec::bare("DROP TABLE session_bindings"));
    statements.push(StatementSpec::bare("DROP TABLE workspaces"));
    statements.push(StatementSpec::bare("DROP TABLE projects"));
    statements.push(StatementSpec::bare(canonical::CREATE_PROJECTS_TABLE_SQL));
    for (project_id, locator) in &project_rows {
        statements.push(StatementSpec::new(
            "INSERT INTO projects (id, locator) VALUES (?1, ?2)",
            vec![
                Value::Text(project_id.clone()),
                Value::Text(locator.clone()),
            ],
        ));
    }
    statements.push(StatementSpec::bare(canonical::CREATE_MACHINES_TABLE_SQL));
    statements.push(current_machine_statement(current_machine));
    statements.push(StatementSpec::bare(canonical::CREATE_WORKSPACES_TABLE_SQL));
    for machine_id in &machines {
        let (name, kind) = crate::sessions::storage_v2_plan::machine_identity(machine_id);
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
        let path = row.path.to_str().ok_or_else(super::dispatch::unsupported)?;
        statements.push(StatementSpec::new(
            INSERT_WORKSPACE_SQL,
            vec![
                Value::Text(row.id.to_string()),
                Value::Text(row.machine_id.clone()),
                Value::Text(path.to_owned()),
                Value::Text(
                    crate::sessions::storage_v2_plan::path_source_name(row.path_source).to_owned(),
                ),
                optional_value(row.project_id.as_deref().map(keep).as_deref()),
                optional_value(row.discovery.as_deref()),
            ],
        ));
    }
    statements.push(StatementSpec::bare(
        "ALTER TABLE threads ADD COLUMN workspace_id TEXT REFERENCES workspaces(id)",
    ));
    statements.push(StatementSpec::bare(
        "ALTER TABLE threads ADD COLUMN archived BOOLEAN NOT NULL DEFAULT 0 CHECK(archived IN (0, 1))",
    ));
    // v14 的两列也必须赶在重建之前：重建语句按当前形状逐列搬运，而输入形状本来没有它们。
    statements.extend(threads_v14_column_statements());
    for session in &sessions {
        let workspace_id = workspace_plan
            .session_workspace_ids
            .get(&session.id)
            .ok_or_else(super::dispatch::unsupported)?;
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
                Value::Text(keep(&row.project_id)),
                Value::Text(row.workspace_id.to_string()),
                Value::Text(row.relative_cwd.clone()),
                Value::Text(row.discovery_snapshot.clone()),
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
    // 派生索引表随 v15 退役：这里不建。
    // `thread_mq` 与索引集里的 `idx_thread_mq_order` 是同一代新增的形状：重建路径不经过
    // `CREATE_TABLES`，必须在这里显式建表，否则末尾的索引重放会撞上「表不存在」。
    statements.push(StatementSpec::bare(canonical::CREATE_THREAD_MQ_TABLE_SQL));
    // 索引重放：`DROP TABLE` 把被重建的表（`threads` / `session_bindings` / `projects` /
    // `workspaces`）上的索引一并删掉，批次末尾把 canonical 的**整份**索引集一次性重放。
    // `IF NOT EXISTS` 让重放幂等（`messages` 的索引本来就在）。
    statements.extend(
        canonical::CREATE_INDEXES
            .iter()
            .map(|sql| StatementSpec::bare(sql)),
    );
    statements.push(advance_version(snapshot));
    Ok(statements)
}

/// v14 → v15 的清理批次：三表重建（删列、收窄唯一键、投影绑定证据）+ 两张退役表 +
/// 索引重放 + 版本推进。
///
/// 与 additive / 搬运路径共用 [`v15_cleanup_statements`]：v14 的库不缺列，只差删除，因此
/// 这里不做 `ALTER`、不搬会话事实；v13/v12/v11 的增量批次在补完列与表之后接同一段语句。
///
/// 守卫链在破坏性语句之前：元数据行、绑定行计数、逐行绑定证据与登记行，任何一条不成立都让
/// 批次以唯一键冲突失败。
pub(in crate::sessions::remote) fn v15_cleanup_plan(
    snapshot: &StoreSnapshot,
    read: &MigrationRead,
    current_machine: &str,
) -> SessionResourceResult<Vec<StatementSpec>> {
    if !v15_cleanup_source(snapshot) {
        return Err(super::dispatch::unsupported());
    }
    let mut statements = vec![StatementSpec::new(
        GUARD_META_SQL,
        vec![
            Value::Integer(snapshot.schema_version),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    )];
    // 绑定行的计数与逐行证据守卫由 `v15_cleanup_statements` 统一下发：additive 与
    // v15 清理共用同一段重建，守卫必须跟着重建走。
    // 登记行只存在于压缩前的输入里；当前形状（v14 → v15）没有它们，不能下发计数守卫。
    if read.has_registration_shape() {
        statements.push(StatementSpec::new(
            GUARD_REGISTRATION_COUNT_SQL,
            vec![Value::Integer(read.registrations.len() as i64)],
        ));
        for row in &read.registrations {
            statements.push(StatementSpec::new(GUARD_REGISTRATION_ROW_SQL, row.clone()));
        }
    }
    statements.extend(v15_cleanup_statements(
        current_machine,
        &read.binding_snapshots,
        &read.projects,
    )?);
    statements.push(advance_version(snapshot));
    Ok(statements)
}

// NOTE: additive 批次在补完 v14 的列与表后接 `v15_cleanup_statements`，一次推进到 v15；
// 绑定守卫也由该语句段统一下发，覆盖 additive 与 v14 清理两条路径。

/// v15 清理的语句段（不含元数据/登记守卫与版本推进）：先下发绑定守卫，再重建三表、
/// 删两表、重放索引。
///
/// `binding_snapshots` 是批外读到的 `(thread_id, discovery_snapshot)` 原文；这里逐行投影成
/// 最小执行根再插入暂存表——SQL 里不做这件事（远端 json1 未实测），两端因此在 Rust 侧共用
/// 同一份投影（`binding_evidence::project`）。解析失败即整批失败：静默降级成 NULL 会把
/// 可执行会话变成只读历史。
pub(super) fn v15_cleanup_statements(
    current_machine: &str,
    binding_snapshots: &[(String, Option<String>)],
    projects: &[(String, String)],
) -> SessionResourceResult<Vec<StatementSpec>> {
    let mut projected = Vec::with_capacity(binding_snapshots.len());
    for (thread_id, snapshot) in binding_snapshots {
        let snapshot = match snapshot.as_deref() {
            None => Value::Null,
            Some(raw) => Value::Text(
                crate::sessions::binding_evidence::project(raw)
                    .map_err(|_| super::dispatch::unsupported())?,
            ),
        };
        projected.push((thread_id.clone(), snapshot));
    }
    // `projects`：保留每个 locator 的 rowid 最早一行（`projects` 已按 rowid 升序读回），
    // 其余行的引用先改绑、后删除。这一步必须在重建之前：重建后的唯一键是
    // `UNIQUE(locator)`，重复行会让搬入直接撞键。
    let mut keep: HashMap<&str, &str> = HashMap::new();
    let mut merges: Vec<(&str, &str)> = Vec::new();
    for (id, locator) in projects {
        match keep.get(locator.as_str()) {
            Some(keep_id) => merges.push((id.as_str(), keep_id)),
            None => {
                keep.insert(locator.as_str(), id.as_str());
            }
        }
    }
    // 守卫链必须在任何破坏性语句之前：重建只搬批外读到的绑定行，批间新增或改写的行
    // 要以唯一键冲突失败，不能被静默丢掉或写入未投影的旧证据。
    let mut statements = vec![StatementSpec::new(
        GUARD_BINDING_COUNT_SQL,
        vec![Value::Integer(binding_snapshots.len() as i64)],
    )];
    for (thread_id, snapshot) in binding_snapshots {
        statements.push(StatementSpec::new(
            GUARD_BINDING_SNAPSHOT_SQL,
            vec![
                Value::Text(thread_id.clone()),
                snapshot.clone().map(Value::Text).unwrap_or(Value::Null),
            ],
        ));
    }
    for (drop_id, keep_id) in &merges {
        statements.push(StatementSpec::new(
            "UPDATE workspaces SET project_id = ?1 WHERE project_id = ?2",
            vec![
                Value::Text((*keep_id).to_owned()),
                Value::Text((*drop_id).to_owned()),
            ],
        ));
        statements.push(StatementSpec::new(
            "UPDATE session_bindings SET project_id = ?1 WHERE project_id = ?2",
            vec![
                Value::Text((*keep_id).to_owned()),
                Value::Text((*drop_id).to_owned()),
            ],
        ));
        statements.push(StatementSpec::new(
            "DELETE FROM projects WHERE id = ?1",
            vec![Value::Text((*drop_id).to_owned())],
        ));
    }
    // 1. projects 重建为 `UNIQUE(locator)`：此时 locator 已唯一，整表搬运即可。
    statements.push(StatementSpec::bare(
        canonical::CREATE_REBUILD_PROJECTS_TABLE_SQL,
    ));
    statements.push(StatementSpec::bare(
        canonical::INSERT_PROJECTS_INTO_REBUILD_SQL,
    ));
    // 2. workspaces：删 `identity`。归属与证据列原样搬运。
    statements.push(StatementSpec::bare(
        canonical::CREATE_REBUILD_WORKSPACES_TABLE_SQL,
    ));
    statements.push(StatementSpec::bare(
        canonical::INSERT_WORKSPACES_INTO_REBUILD_SQL,
    ));
    // 3. session_bindings：删 `evidence_origin`，证据逐行投影。
    statements.push(StatementSpec::bare(
        canonical::CREATE_REBUILD_BINDINGS_TABLE_SQL,
    ));
    for (thread_id, snapshot) in &projected {
        statements.push(StatementSpec::new(
            "INSERT INTO session_bindings_v15 (thread_id, schema_version, project_id, workspace_id, relative_cwd, discovery_snapshot)
             SELECT thread_id, schema_version, project_id, workspace_id, relative_cwd, ?2
             FROM session_bindings WHERE thread_id = ?1",
            vec![Value::Text(thread_id.clone()), snapshot.clone()],
        ));
    }
    // 4. 换表：先删引用者，再删父表，最后按 v15 DDL 建新表并搬回。
    statements.push(StatementSpec::bare("DROP TABLE session_bindings"));
    statements.push(StatementSpec::bare("DROP TABLE workspaces"));
    statements.push(StatementSpec::bare("DROP TABLE projects"));
    statements.push(StatementSpec::bare(canonical::CREATE_PROJECTS_TABLE_SQL));
    statements.push(StatementSpec::bare(canonical::CREATE_WORKSPACES_TABLE_SQL));
    statements.push(StatementSpec::bare(canonical::CREATE_BINDINGS_TABLE_SQL));
    statements.push(current_machine_statement(current_machine));
    statements.push(StatementSpec::bare(
        canonical::INSERT_REBUILD_INTO_PROJECTS_SQL,
    ));
    statements.push(StatementSpec::bare(
        canonical::INSERT_REBUILD_INTO_WORKSPACES_SQL,
    ));
    statements.push(StatementSpec::bare(
        canonical::INSERT_REBUILD_INTO_BINDINGS_SQL,
    ));
    statements.extend(
        [
            "DROP TABLE projects_v15",
            "DROP TABLE workspaces_v15",
            "DROP TABLE session_bindings_v15",
        ]
        .into_iter()
        .map(StatementSpec::bare),
    );
    // 5. 退役的两张派生索引表（payload 先于 snapshot）。缺表时 `IF EXISTS` 幂等。
    statements.extend(
        canonical::DROP_INHERITED_INDEX_TABLES
            .iter()
            .map(|sql| StatementSpec::bare(sql)),
    );
    // 6. 索引重放：重建的三张表随 `DROP TABLE` 丢掉了自己的索引。
    statements.extend(
        canonical::CREATE_INDEXES
            .iter()
            .map(|sql| StatementSpec::bare(sql)),
    );
    Ok(statements)
}
pub(super) fn current_machine_statement(current_machine: &str) -> StatementSpec {
    StatementSpec::new(
        canonical::INSERT_CURRENT_MACHINE_SQL,
        vec![Value::Text(current_machine.to_owned())],
    )
}

/// v14 的 `threads` 增量列：增量批次与搬运重建都要在收尾前补齐，DDL 文本只在这里生成。
pub(super) fn threads_v14_column_statements() -> impl Iterator<Item = StatementSpec> {
    canonical::THREADS_V14_COLUMNS
        .iter()
        .map(|(name, definition)| {
            StatementSpec::bare(Box::leak(
                format!("ALTER TABLE threads ADD COLUMN {name} {definition}").into_boxed_str(),
            ))
        })
}

/// 版本与契约一起推进：形状变了，代数跟着变。
pub(super) fn advance_version(snapshot: &StoreSnapshot) -> StatementSpec {
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

pub(super) fn object_values(object: &SchemaObject) -> Vec<Value> {
    vec![
        Value::Text(object.kind.clone()),
        Value::Text(object.name.clone()),
        Value::Text(object.table.clone()),
        object.sql.clone().map(Value::Text).unwrap_or(Value::Null),
    ]
}

pub(super) fn optional_value(value: Option<&str>) -> Value {
    match value {
        Some(value) => Value::Text(value.to_owned()),
        None => Value::Null,
    }
}
