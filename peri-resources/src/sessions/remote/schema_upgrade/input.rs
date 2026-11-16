//! Consistent migration input reads and decoding of legacy rows.

use super::super::mutation::RemoteStore;
use super::super::schema;
use super::super::schema::StoreSnapshot;
use super::super::sql::StatementSpec;
use super::statements::{
    MESSAGES_COUNT_SQL, READ_BINDINGS_SQL, READ_BINDING_SNAPSHOTS_SQL, READ_PROJECTS_SQL,
    READ_REGISTRATIONS_SQL, READ_SESSIONS_SQL, READ_SESSIONS_WITHOUT_ENVIRONMENTS_SQL,
};
use crate::sessions::schema_cleanup;
use crate::sessions::schema_cleanup::ColumnShape;
use crate::sessions::schema_cleanup::SchemaObject;
use crate::sessions::schema_shape;
use crate::sessions::storage_v2_plan::LegacyBindingRow;
use crate::sessions::storage_v2_plan::LegacyRegistration;
use crate::sessions::storage_v2_plan::LegacySession;
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::workspace::WorkspaceId;
use peri_acp_types::workspace::WorkspacePathSource;
use std::collections::HashMap;
use turso_serverless::Value;

pub(super) fn optional_text(row: &[Value], index: usize) -> SessionResourceResult<Option<String>> {
    match row.get(index) {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(value)) => Ok(Some(value.clone())),
        _ => Err(super::dispatch::unsupported()),
    }
}

pub(super) fn optional_string(
    row: &[Value],
    index: usize,
) -> SessionResourceResult<Option<String>> {
    optional_text(row, index)
}

/// `sqlite_master` 行 → 对象定义（守卫逐对象比对时用同一份值）。
pub(super) fn decode_objects(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<SchemaObject>> {
    rows.iter()
        .map(|row| {
            Ok(SchemaObject {
                kind: super::super::sql::text_at(row, 0)
                    .ok_or_else(super::dispatch::unsupported)?
                    .to_owned(),
                name: super::super::sql::text_at(row, 1)
                    .ok_or_else(super::dispatch::unsupported)?
                    .to_owned(),
                table: super::super::sql::text_at(row, 2)
                    .ok_or_else(super::dispatch::unsupported)?
                    .to_owned(),
                sql: optional_string(row, 3)?,
            })
        })
        .collect()
}

pub(super) fn decode_columns(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<ColumnShape>> {
    rows.iter()
        .map(|row| {
            Ok(ColumnShape {
                name: super::super::sql::text_at(row, 1)
                    .ok_or_else(super::dispatch::unsupported)?
                    .to_owned(),
                kind: super::super::sql::text_at(row, 2)
                    .ok_or_else(super::dispatch::unsupported)?
                    .to_owned(),
                not_null: super::super::sql::int_at(row, 3)
                    .ok_or_else(super::dispatch::unsupported)?,
                default: optional_string(row, 4)?,
                primary_key: super::super::sql::int_at(row, 5)
                    .ok_or_else(super::dispatch::unsupported)?,
                hidden: super::super::sql::int_at(row, 6)
                    .ok_or_else(super::dispatch::unsupported)?,
            })
        })
        .collect()
}

/// 一次一致读取得到的迁移输入（原始行 + 形状快照）。
pub(in crate::sessions::remote) struct MigrationRead {
    pub(super) sessions: Vec<Vec<Value>>,
    pub(super) bindings: Vec<Vec<Value>>,
    pub(super) registrations: Vec<Vec<Value>>,
    /// 绑定行的创建证据原文：`(thread_id, discovery_snapshot)`。v15 清理要在 Rust 侧把它
    /// 投影成最小执行根；`None` 表示该行本来就没有证据。
    pub(super) binding_snapshots: Vec<(String, Option<String>)>,
    pub(super) objects: Vec<SchemaObject>,
    /// `threads` 的完整列形状（含类型与默认值）：退役缓存列的判定要它，不是迁移输入判定。
    pub(super) thread_columns: Vec<ColumnShape>,
    /// 迁移输入各表的列形状（逐表 PRAGMA，沿用本机迁移的输入形状声明）。
    pub(super) input_columns: HashMap<String, Vec<schema_shape::ActualColumn>>,
    /// 要删除对象的列形状（含 v15 退役的两张 inherited 表）：这些表不在输入读取面里，
    /// 单独读一次供 `check_dropped_shape` 使用。
    pub(super) dropped_columns: HashMap<String, Vec<schema_shape::ActualColumn>>,
    /// 项目行 `(id, locator)`，按 rowid 升序：v15 合并重复 locator 时保留最早一行。
    pub(super) projects: Vec<(String, String)>,
    /// 搬到之前的 `messages` 行数（结果守卫的基准）。`None` = 输入形状里没有这张表——
    /// 那时形状判定会以「不认识的输入」拒绝整条迁移，守卫本身不会被构造。
    pub(super) messages_count: Option<i64>,
}

impl MigrationRead {
    /// 迁移输入里是否存在 `session_environments`：它是可选表（正式基线 10 的库可能没有），
    /// 会话行与逐行守卫的读法按它在不在分岔。
    pub(super) fn has_environments(&self) -> bool {
        self.declares("session_environments")
    }

    /// 第一批读到的形状里是否有这张表（缺表在结果里没有行，与「空形状」不同）。
    pub(super) fn declares(&self, table: &str) -> bool {
        self.input_columns
            .get(table)
            .is_some_and(|columns| !columns.is_empty())
    }

    /// 输入是否带**压缩前**的登记形状（`workspaces.root`）。
    ///
    /// 当前形状的 `workspaces` 用 `path`，不具备登记语义；两者共用表名，因此按列判定。
    pub(super) fn has_registration_shape(&self) -> bool {
        self.input_columns.get("workspaces").is_some_and(|columns| {
            columns
                .iter()
                .any(|column| column.name.eq_ignore_ascii_case("root"))
        })
    }
}

/// 迁移输入的一致读取：形状一批（对象定义与列清单，任何库都读得动），有会话表时再读一批
/// 搬运输入。两批之间没有事务，漂移交给批次里的守卫拦下。
///
/// 生产（[`upgrade`]）与批次构造的离线断言共用这一处读法：批次里的守卫按这里读到的事实
/// 复核，两者因此逐列同源。
pub(in crate::sessions::remote) async fn read_input(
    store: &RemoteStore,
) -> SessionResourceResult<MigrationRead> {
    let mut statements = vec![StatementSpec::bare(schema_cleanup::SCHEMA_OBJECTS_SQL)];
    for table in schema_shape::INPUT_TABLES {
        let sql = schema::ShapeProbe::pragma_table_xinfo(table.name).ok_or_else(|| {
            super::super::mutation::incomplete_reply("remote upgrade input table is not supported")
        })?;
        statements.push(StatementSpec::bare(sql));
    }
    // 要删除的对象（含 v15 退役的两张 inherited 表）不在输入读取面里。这些表在合法输入里
    // **本来就可以不存在**（远端从未建过派生索引），而 `PRAGMA table_xinfo` 对缺表会让整个
    // 读批次失败——所以先单独探一次存在性，只对存在的表补读形状。
    let retired = [
        "inherited_snapshot_index",
        "inherited_payload_index",
        "projects",
    ];
    let presence = store
        .read_batch(
            retired
                .iter()
                .map(|table| {
                    StatementSpec::new(
                        schema::TABLE_EXISTS_SQL,
                        vec![Value::Text((*table).to_owned())],
                    )
                })
                .collect(),
        )
        .await?;
    let present: Vec<&str> = retired
        .iter()
        .zip(presence.iter())
        .filter_map(|(table, rows)| (!rows.is_empty()).then_some(*table))
        .collect();
    // `projects` 只为 v15 的重复 locator 合并而读；它不在迁移输入面里（输入面只有属于
    // 搬运的表），也不在删除面里。
    let has_projects = present.contains(&"projects");
    let dropped_names: Vec<&str> = present
        .iter()
        .copied()
        .filter(|name| *name != "projects")
        .chain(
            schema_shape::DROPPED_TABLES
                .iter()
                .map(|table| table.shape.name)
                .filter(|name| !retired.contains(name)),
        )
        .collect();
    for name in &dropped_names {
        let sql = schema::ShapeProbe::pragma_table_xinfo(name).ok_or_else(|| {
            super::super::mutation::incomplete_reply("remote dropped table is not supported")
        })?;
        statements.push(StatementSpec::bare(sql));
    }
    let shape = store.read_batch(statements).await?;
    let mut columns = Vec::new();
    let input_tables = schema_shape::INPUT_TABLES.iter().zip(shape.iter().skip(1));
    for (table, rows) in input_tables {
        for row in rows {
            let (Some(name), Some(not_null), Some(primary_key)) =
                (row.get(1), row.get(3), row.get(5))
            else {
                return Err(super::super::mutation::incomplete_reply(
                    "remote upgrade input returned an incomplete column",
                ));
            };
            columns.push(vec![
                Value::Text(table.name.to_owned()),
                name.clone(),
                not_null.clone(),
                primary_key.clone(),
            ]);
        }
    }
    let dropped_offset = 1 + schema_shape::INPUT_TABLES.len();
    let mut dropped_columns = Vec::new();
    let paired_entries = dropped_names.iter().zip(shape.iter().skip(dropped_offset));
    for (table, rows) in paired_entries {
        for row in rows {
            let (Some(name), Some(not_null), Some(primary_key)) =
                (row.get(1), row.get(3), row.get(5))
            else {
                return Err(super::super::mutation::incomplete_reply(
                    "remote dropped table returned an incomplete column",
                ));
            };
            dropped_columns.push(vec![
                Value::Text((*table).to_owned()),
                name.clone(),
                not_null.clone(),
                primary_key.clone(),
            ]);
        }
    }
    let mut read = MigrationRead {
        sessions: Vec::new(),
        bindings: Vec::new(),
        registrations: Vec::new(),
        binding_snapshots: Vec::new(),
        objects: decode_objects(&shape[0])?,
        thread_columns: decode_columns(&shape[1])?,
        input_columns: schema::decode_column_map(&columns)?,
        dropped_columns: schema::decode_column_map(&dropped_columns)?,
        projects: Vec::new(),
        messages_count: None,
    };
    // v2 的绑定形状没有 `discovery_snapshot` 列（压缩前的输入只在 v11 起才有）。
    let has_binding_snapshot = read
        .input_columns
        .get("session_bindings")
        .is_some_and(|columns| {
            columns
                .iter()
                .any(|column| column.name.eq_ignore_ascii_case("discovery_snapshot"))
        });
    if read.thread_columns.is_empty() {
        return Ok(read);
    }
    let sessions_sql = if read.has_environments() {
        READ_SESSIONS_SQL
    } else {
        READ_SESSIONS_WITHOUT_ENVIRONMENTS_SQL
    };
    let mut statements = vec![StatementSpec::bare(sessions_sql)];
    statements.push(StatementSpec::bare(READ_BINDINGS_SQL));
    // 登记行只存在于压缩前的形状（`workspaces.root`）。当前形状的 `workspaces` 用
    // `path`，这条读法会在它上面报「no such column」——因此按列判断，而不是按表名。
    let has_registration_shape = read.has_registration_shape();
    if has_registration_shape {
        statements.push(StatementSpec::bare(READ_REGISTRATIONS_SQL));
    }
    if has_binding_snapshot {
        statements.push(StatementSpec::bare(READ_BINDING_SNAPSHOTS_SQL));
    }
    if has_projects {
        statements.push(StatementSpec::bare(READ_PROJECTS_SQL));
    }
    let rows = store.read_batch(statements).await?;
    let mut next = 2;
    read.sessions = rows[0].clone();
    read.bindings = rows[1].clone();
    if has_registration_shape {
        read.registrations = rows[next].clone();
        next += 1;
    }
    if has_binding_snapshot {
        read.binding_snapshots = rows[next]
            .iter()
            .map(|row| {
                Ok((
                    super::super::sql::text_at(row, 0)
                        .ok_or_else(super::dispatch::unsupported)?
                        .to_owned(),
                    optional_text(row, 1)?,
                ))
            })
            .collect::<SessionResourceResult<Vec<_>>>()?;
        next += 1;
    }
    if has_projects {
        let projects = rows.get(next).ok_or_else(|| {
            super::super::mutation::incomplete_reply(
                "remote upgrade input returned no project rows",
            )
        })?;
        read.projects = projects
            .iter()
            .map(|row| {
                Ok((
                    super::super::sql::text_at(row, 0)
                        .ok_or_else(super::dispatch::unsupported)?
                        .to_owned(),
                    super::super::sql::text_at(row, 1)
                        .ok_or_else(super::dispatch::unsupported)?
                        .to_owned(),
                ))
            })
            .collect::<SessionResourceResult<Vec<_>>>()?;
    }
    // 历史行数只在这张表确实存在时读（与 `has_environments` 同一条规则）：表缺席时形状判定
    // 会以 `Unsupported` 拒绝整条迁移，这里不能把那个拒绝降级成一次「表不存在」的读失败。
    if read.declares("messages") {
        let row = store
            .fetch_row(&StatementSpec::bare(MESSAGES_COUNT_SQL))
            .await?;
        // 「读不到计数」不是「没有历史」：按不完整读上报，绝不让守卫以 0 为基准放行。
        let count = row
            .as_ref()
            .and_then(|values| super::super::sql::int_at(values, 0))
            .ok_or_else(|| {
                super::super::mutation::incomplete_reply("message count read returned no count")
            })?;
        read.messages_count = Some(count);
    }
    Ok(read)
}

/// 会话行与绑定行 → 规划输入。
///
/// 会话的机器归属缺行时用 `legacy:<store id>` 兜底（本机以外的旧机器，与未知机器同类）；
/// 绑定记录的根取不到时按保存的 cwd 规划，规划只用于分组，绝不当成执行证据。
pub(super) fn decode_legacy(
    session_rows: &[Vec<Value>],
    binding_rows: &[Vec<Value>],
    registration_rows: &[Vec<Value>],
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<(Vec<LegacySession>, Vec<LegacyRegistration>)> {
    let registrations = decode_registrations(registration_rows)?;
    let mut bindings: std::collections::HashMap<String, (String, Option<String>)> =
        std::collections::HashMap::new();
    for row in binding_rows {
        let thread_id = super::super::sql::text_at(row, 0)
            .ok_or_else(super::dispatch::unsupported)?
            .to_owned();
        // 绑定表的主键是 `thread_id`：读回重复行说明这不是本构建认识的形状（例如同名的
        // VIEW）。后行覆盖前行会让输出比输入少一行、版本却照常推进——显式拒绝。
        let replaced = bindings.insert(
            thread_id,
            (
                super::super::sql::text_at(row, 3)
                    .ok_or_else(super::dispatch::unsupported)?
                    .to_owned(),
                optional_text(row, 4)?,
            ),
        );
        if replaced.is_some() {
            return Err(super::dispatch::unsupported());
        }
    }
    let mut sessions = Vec::with_capacity(session_rows.len());
    for row in session_rows {
        let id = super::super::sql::text_at(row, 0)
            .ok_or_else(super::dispatch::unsupported)?
            .to_owned();
        let parent_id = optional_text(row, 1)?;
        let cwd = super::super::sql::text_at(row, 2)
            .ok_or_else(super::dispatch::unsupported)?
            .to_owned();
        let machine_id = optional_text(row, 3)?
            .unwrap_or_else(|| format!("legacy:{}", snapshot.store_id.as_str()));
        let binding = bindings.get(&id);
        let registration = binding
            .map(|(workspace_id, _)| {
                workspace_id
                    .parse::<WorkspaceId>()
                    .map_err(|_| super::dispatch::unsupported())
            })
            .transpose()?;
        // 相对路径与保存的 cwd 推不出同一个根时，绑定记录的根不再可信：
        // 按保存的 cwd 分组（规划用途），绑定本身仍原样搬运并原样校验。
        let derived_root = binding
            .and_then(|(_, relative)| relative.as_deref())
            .and_then(|relative| {
                crate::sessions::storage_v2_plan::derive_remote_root(&cwd, relative).ok()
            });
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
                return Err(super::dispatch::unsupported());
            }
        }
    }
    Ok((sessions, registrations))
}

/// 登记行 → 规划输入：观测证据必须描述同一个根，否则这不是本构建认识的登记行。
pub(super) fn decode_registrations(
    rows: &[Vec<Value>],
) -> SessionResourceResult<Vec<LegacyRegistration>> {
    let mut registrations = Vec::with_capacity(rows.len());
    for row in rows {
        let id: WorkspaceId = super::super::sql::text_at(row, 0)
            .ok_or_else(super::dispatch::unsupported)?
            .parse()
            .map_err(|_| super::dispatch::unsupported())?;
        let project_id = super::super::sql::text_at(row, 1)
            .ok_or_else(super::dispatch::unsupported)?
            .to_owned();
        let root = super::super::sql::text_at(row, 2)
            .ok_or_else(super::dispatch::unsupported)?
            .to_owned();
        let discovery = super::super::sql::text_at(row, 3)
            .ok_or_else(super::dispatch::unsupported)?
            .to_owned();
        let observed: serde_json::Value =
            serde_json::from_str(&discovery).map_err(|_| super::dispatch::unsupported())?;
        if observed.get("root").and_then(serde_json::Value::as_str) != Some(root.as_str()) {
            return Err(super::dispatch::unsupported());
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
            discovery,
        });
    }
    Ok(registrations)
}

/// 绑定行 → 搬运输入。
pub(super) fn decode_bindings(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<LegacyBindingRow>> {
    rows.iter()
        .map(|row| {
            Ok(LegacyBindingRow {
                thread_id: super::super::sql::text_at(row, 0)
                    .ok_or_else(super::dispatch::unsupported)?
                    .to_owned(),
                schema_version: super::super::sql::int_at(row, 1)
                    .ok_or_else(super::dispatch::unsupported)?,
                project_id: super::super::sql::text_at(row, 2)
                    .ok_or_else(super::dispatch::unsupported)?
                    .to_owned(),
                workspace_id: super::super::sql::text_at(row, 3)
                    .ok_or_else(super::dispatch::unsupported)?
                    .parse()
                    .map_err(|_| super::dispatch::unsupported())?,
                relative_cwd: super::super::sql::text_at(row, 4)
                    .ok_or_else(super::dispatch::unsupported)?
                    .to_owned(),
            })
        })
        .collect()
}
