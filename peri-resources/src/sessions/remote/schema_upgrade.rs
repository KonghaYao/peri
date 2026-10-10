//! 压缩后的远端单条迁移：压缩前的存储契约（`v2`，版本 10|11）→ 当前契约（`v5`，版本 11）。
//!
//! 远端与本地共用同一份搬运**规则**（`sessions::storage_v2_plan`）与同一份目标 DDL
//! （`sessions::canonical`），差别只在执行器：远端把整段迁移作为**一次受管批次**下发。
//! 这里因此不重写迁移语义，只补两样远端特有的东西：
//!
//! - **守卫**：形状快照、逐对象定义、逐行会话与绑定核对插在批次最前，任何一条不成立都让
//!   批次以唯一键冲突失败——读到形状与提交时形状必须一致，迁移不作用在漂移过的库上。
//!   本机在事务内读回判定，远端没有事务内读回，只能把同一条规则换成本批次的载体
//!   （`INSERT ... SELECT 0 WHERE (<核对>) <> 0`）。
//! - **版本与契约推进**：作为批次最后一条，把 `(v2, 10|11)` 一次推进到
//!   [`schema::STORE_CONTRACT`] 的当前代数——形状变了，代数跟着变，旧构建凭契约标签拒绝。
//!
//! 读分两批：形状一批（对象定义与列清单，任何库都读得动），有会话表时再读一批搬运输入。
//! 空库（v2 契约但还没有 canonical 表）是同一路径的退化：没有可搬运的行，直接建当前形状。
//! 两批之间没有事务，漂移交给批次里的守卫拦下。

use std::collections::BTreeSet;

use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::workspace::{WorkspaceId, WorkspacePathSource};
use turso_serverless::Value;

use super::mutation::RemoteStore;
use super::schema::{self, StoreSnapshot};
use super::sql::{int_at, text_at, StatementSpec};
use crate::sessions::canonical;
use crate::sessions::schema_cleanup::{self, ColumnShape, SchemaObject};
use crate::sessions::storage_v2_plan::{
    derive_remote_root, machine_identity, path_source_name, plan_binding_rows,
    plan_local_workspaces, plan_workspace_rows, LegacyBindingRow, LegacyRegistration,
    LegacySession,
};

/// 会话行：机器归属可能缺行（旧版打开路径不写环境表）。
const READ_SESSIONS_SQL: &str = "SELECT t.id, t.parent_thread_id, t.cwd, e.machine_id
    FROM threads t LEFT JOIN session_environments e ON e.thread_id = t.id ORDER BY t.id";
/// 绑定行：`workspace_id` 指向执行登记，不是归属行。
const READ_BINDINGS_SQL: &str =
    "SELECT thread_id, schema_version, project_id, workspace_id, relative_cwd
    FROM session_bindings ORDER BY thread_id";
/// 登记行：归属证据随行搬到当前形状的归属行。
const READ_REGISTRATIONS_SQL: &str =
    "SELECT id, project_id, root, root_identity, discovery FROM workspaces ORDER BY id";
const READ_WORKSPACE_COLUMNS_SQL: &str =
    "SELECT name FROM pragma_table_info('workspaces') ORDER BY cid";
const READ_BINDING_COLUMNS_SQL: &str =
    "SELECT name FROM pragma_table_info('session_bindings') ORDER BY cid";

/// 压缩前形状里 `workspaces` 必须是登记表：缺列说明这不是本构建认识的形状，拒绝而不是猜。
const REGISTRATION_COLUMNS: &[&str] = &["id", "project_id", "root", "root_identity", "discovery"];
/// 迁移输入里绑定表必须有的列。
const BINDING_COLUMNS: &[&str] = &[
    "thread_id",
    "schema_version",
    "project_id",
    "workspace_id",
    "relative_cwd",
];

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
const GUARD_BINDING_COUNT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM session_bindings) <> ?1";
const GUARD_BINDING_ROW_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM session_bindings
    WHERE thread_id = ?1 AND schema_version = ?2 AND project_id = ?3 AND workspace_id = ?4 AND relative_cwd = ?5)";
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
    thread_columns: Vec<ColumnShape>,
    message_columns: Vec<ColumnShape>,
    workspace_columns: Vec<String>,
    binding_columns: Vec<String>,
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
    // 形状先读：空库（只有身份与账本）在第一批就能判定，不去读还不存在的会话表。
    let shape = store
        .read_batch(vec![
            StatementSpec::bare(schema_cleanup::SCHEMA_OBJECTS_SQL),
            StatementSpec::bare(schema_cleanup::THREAD_COLUMNS_SQL),
            StatementSpec::bare(schema_cleanup::MESSAGE_COLUMNS_SQL),
            StatementSpec::bare(READ_WORKSPACE_COLUMNS_SQL),
            StatementSpec::bare(READ_BINDING_COLUMNS_SQL),
        ])
        .await?;
    let mut read = MigrationRead {
        sessions: Vec::new(),
        bindings: Vec::new(),
        registrations: Vec::new(),
        objects: decode_objects(&shape[0])?,
        thread_columns: decode_columns(&shape[1])?,
        message_columns: decode_columns(&shape[2])?,
        workspace_columns: column_names(&shape[3])?,
        binding_columns: column_names(&shape[4])?,
    };
    // 有会话表：第二批读齐搬运输入。两批之间库可能漂移，批次里的守卫按「第一批读到的
    // 形状 + 第二批读到的行」逐条复核，任何一条不成立都让整批失败。
    if !read.thread_columns.is_empty() {
        let rows = store
            .read_batch(vec![
                StatementSpec::bare(READ_SESSIONS_SQL),
                StatementSpec::bare(READ_BINDINGS_SQL),
                StatementSpec::bare(READ_REGISTRATIONS_SQL),
            ])
            .await?;
        read.sessions = rows[0].clone();
        read.bindings = rows[1].clone();
        read.registrations = rows[2].clone();
    }
    // 当前 Machine 也要在批次里立起来（与本机迁移同一条语句）。写打开在此之前从不需要
    // 本机身份，所以这里显式初始化一次：拿不到身份就不发批次。
    crate::sessions::machine::initialize()
        .await
        .map_err(|_| unsupported())?;
    let current_machine = crate::sessions::machine::current().map_err(|_| unsupported())?;
    store
        .apply_schema_upgrade(upgrade_plan(snapshot, &read, current_machine)?)
        .await
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

    if !canonical::THREAD_COLUMN_NAMES.iter().all(|required| {
        read.thread_columns
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case(required))
    }) || !canonical::MESSAGE_COLUMN_NAMES.iter().all(|required| {
        read.message_columns
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case(required))
    }) || !required_columns(REGISTRATION_COLUMNS, &read.workspace_columns)
        || !required_columns(BINDING_COLUMNS, &read.binding_columns)
    {
        return Err(unsupported());
    }
    let removals = schema_cleanup::removal_plan(&read.objects, &read.thread_columns)
        .map_err(|_| unsupported())?;
    let (sessions, registrations) = decode_legacy(
        &read.sessions,
        &read.bindings,
        &read.registrations,
        snapshot,
    )?;
    let legacy_bindings = decode_bindings(&read.bindings)?;
    let workspace_plan =
        plan_local_workspaces(&sessions, &registrations).map_err(|_| unsupported())?;
    if workspace_plan.session_workspace_ids.len() != sessions.len() {
        return Err(unsupported());
    }
    let machines: BTreeSet<String> = workspace_plan
        .workspaces
        .iter()
        .map(|workspace| workspace.machine_id.clone())
        .collect();
    let workspace_rows = plan_workspace_rows(&workspace_plan, &sessions, &registrations, &machines)
        .map_err(|_| unsupported())?;
    let binding_rows = plan_binding_rows(&legacy_bindings, &workspace_plan, &registrations)
        .map_err(|_| unsupported())?;

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
    for row in &read.sessions {
        statements.push(StatementSpec::new(GUARD_THREAD_ROW_SQL, row.clone()));
    }
    statements.push(StatementSpec::new(
        GUARD_BINDING_COUNT_SQL,
        vec![Value::Integer(read.bindings.len() as i64)],
    ));
    for row in &read.bindings {
        statements.push(StatementSpec::new(GUARD_BINDING_ROW_SQL, row.clone()));
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
    statements.push(StatementSpec::bare(canonical::THREAD_WORKSPACE_INDEX));
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
    statements.extend(
        canonical::BINDING_INDEXES
            .iter()
            .map(|sql| StatementSpec::bare(sql)),
    );
    // 旧凭证只有 machine 作用域，无法证明属于哪个 Workspace。
    statements.push(StatementSpec::bare("DROP TABLE session_environments"));
    statements.push(StatementSpec::bare("DROP TABLE mcp_oauth_credentials"));
    statements.push(StatementSpec::bare(
        canonical::CREATE_OAUTH_CREDENTIALS_TABLE_SQL,
    ));
    statements.push(StatementSpec::bare(
        canonical::CREATE_SESSION_CLOSE_INTENTS_TABLE_SQL,
    ));
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

fn required_columns(required: &[&str], actual: &[String]) -> bool {
    required
        .iter()
        .all(|column| actual.iter().any(|name| name.eq_ignore_ascii_case(column)))
}

fn column_names(rows: &[Vec<Value>]) -> SessionResourceResult<Vec<String>> {
    rows.iter()
        .map(|row| Ok(text_at(row, 0).ok_or_else(unsupported)?.to_owned()))
        .collect()
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
        bindings.insert(
            text_at(row, 0).ok_or_else(unsupported)?.to_owned(),
            (
                text_at(row, 3).ok_or_else(unsupported)?.to_owned(),
                optional_text(row, 4)?,
            ),
        );
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
