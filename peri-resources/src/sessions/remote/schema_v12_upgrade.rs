//! Guarded remote 11→12 data migration. The plan is built from one consistent
//! snapshot, then every source row is rechecked inside the managed write batch.

use std::{collections::BTreeSet, path::PathBuf};

use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::workspace::{WorkspaceId, WorkspacePathSource};
use turso_serverless::Value;

use super::{
    mutation::RemoteStore,
    schema::StoreSnapshot,
    sql::{text_at, StatementSpec},
};
use crate::sessions::{
    canonical,
    sqlite_store::storage_v2_plan::{
        derive_remote_root, plan_local_workspaces, LegacyRegistration, LegacySession,
    },
};

const READ_SESSIONS_SQL: &str =
    "SELECT t.id, t.parent_thread_id, t.cwd, e.machine_id, b.workspace_id, b.relative_cwd
    FROM threads t LEFT JOIN session_environments e ON e.thread_id = t.id
    LEFT JOIN session_bindings b ON b.thread_id = t.id ORDER BY t.id";
const READ_REGISTRATIONS_SQL: &str = "SELECT id, root, discovery FROM workspaces ORDER BY id";
const READ_THREAD_COLUMNS_SQL: &str = "SELECT name FROM pragma_table_info('threads')";
const READ_SCHEMA_OBJECTS_SQL: &str = "SELECT type, name, tbl_name, sql FROM sqlite_master
    WHERE substr(lower(name), 1, 7) <> 'sqlite_' ORDER BY type, name";
const GUARD_OBJECT_COUNT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM sqlite_master WHERE substr(lower(name), 1, 7) <> 'sqlite_') <> ?1";
const GUARD_OBJECT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS
    (SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2 AND tbl_name = ?3 AND sql IS ?4)";
const GUARD_META_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS
    (SELECT 1 FROM peri_store_meta WHERE singleton = 0 AND schema_version = 11 AND store_id = ?1 AND contract = ?2)";
const GUARD_COUNT_SQL: &str =
    "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE (SELECT COUNT(*) FROM threads) <> ?1";
const GUARD_ROW_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE NOT EXISTS (
    SELECT 1 FROM threads t LEFT JOIN session_environments e ON e.thread_id = t.id
    LEFT JOIN session_bindings b ON b.thread_id = t.id
    WHERE t.id = ?1 AND t.parent_thread_id IS ?2 AND t.cwd = ?3
      AND e.machine_id IS ?4 AND b.workspace_id IS ?5 AND b.relative_cwd IS ?6)";
const COPY_THREAD_SQL: &str = "INSERT INTO threads_v12 (
    id, title, cwd, created_at, updated_at, message_count, parent_thread_id,
    snapshot_at_message_id, hidden, cancel_policy, config, frozen_context,
    inherited_context, agent_status, workspace_id, archived)
    SELECT id, title, cwd, created_at, updated_at, message_count, parent_thread_id,
    snapshot_at_message_id, hidden, cancel_policy, config, frozen_context,
    inherited_context, agent_status, ?2, 0 FROM threads WHERE id = ?1";
const GUARD_TARGET_COUNT_SQL: &str = "INSERT INTO peri_store_meta(singleton) SELECT 0 WHERE
    (SELECT COUNT(*) FROM threads_v12) <> ?1";
const INSERT_MACHINE_SQL: &str =
    "INSERT INTO machines(id, name, identity_kind) VALUES (?1, ?2, ?3)";
const INSERT_WORKSPACE_SQL: &str =
    "INSERT INTO workspaces(id, machine_id, path, path_source) VALUES (?1, ?2, ?3, ?4)";
const UPDATE_META_SQL: &str = "UPDATE peri_store_meta SET schema_version = 12, contract = ?1
    WHERE singleton = 0 AND schema_version = 11 AND store_id = ?2 AND contract = ?3";

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

pub(super) async fn upgrade(
    store: &RemoteStore,
    snapshot: &StoreSnapshot,
) -> SessionResourceResult<()> {
    if snapshot.schema_version != 11 || snapshot.contract != "peri.session.store/v2" {
        return Err(unsupported());
    }
    let results = store
        .read_batch(vec![
            StatementSpec::bare(READ_SESSIONS_SQL),
            StatementSpec::bare(READ_REGISTRATIONS_SQL),
            StatementSpec::bare(READ_THREAD_COLUMNS_SQL),
            StatementSpec::bare(READ_SCHEMA_OBJECTS_SQL),
        ])
        .await?;
    let old_columns: BTreeSet<_> = canonical::THREAD_COLUMN_NAMES.iter().copied().collect();
    let actual_columns: BTreeSet<_> = results[2]
        .iter()
        .map(|row| text_at(row, 0).ok_or_else(unsupported))
        .collect::<SessionResourceResult<_>>()?;
    if actual_columns != old_columns {
        return Err(unsupported());
    }
    for object in &results[3] {
        let kind = text_at(object, 0).ok_or_else(unsupported)?;
        let name = text_at(object, 1).ok_or_else(unsupported)?;
        let table = text_at(object, 2).ok_or_else(unsupported)?;
        let sql = optional_text(object, 3)?;
        if (kind == "index" && table == "threads" && name != "idx_threads_updated")
            || (kind == "table"
                && !["messages", "session_bindings", "session_environments"].contains(&name)
                && sql
                    .as_deref()
                    .is_some_and(|sql| sql.to_ascii_lowercase().contains("references threads")))
        {
            return Err(unsupported());
        }
    }
    let mut registrations = Vec::new();
    for row in &results[1] {
        let id: WorkspaceId = text_at(row, 0)
            .ok_or_else(unsupported)?
            .parse()
            .map_err(|_| unsupported())?;
        let root = text_at(row, 1).ok_or_else(unsupported)?;
        let discovery: serde_json::Value =
            serde_json::from_str(text_at(row, 2).ok_or_else(unsupported)?)
                .map_err(|_| unsupported())?;
        if discovery.get("root").and_then(serde_json::Value::as_str) != Some(root) {
            return Err(unsupported());
        }
        let source = if discovery
            .get("common_dir")
            .is_some_and(|value| !value.is_null())
            || discovery
                .get("private_dir")
                .is_some_and(|value| !value.is_null())
        {
            WorkspacePathSource::Discovered
        } else {
            WorkspacePathSource::Unverified
        };
        registrations.push(LegacyRegistration {
            id,
            root: PathBuf::from(root),
            path_source: source,
        });
    }
    let mut sessions = Vec::new();
    for row in &results[0] {
        let id = text_at(row, 0).ok_or_else(unsupported)?.to_owned();
        let parent_id = optional_text(row, 1)?;
        let cwd = text_at(row, 2).ok_or_else(unsupported)?.to_owned();
        let machine_id = optional_text(row, 3)?
            .unwrap_or_else(|| format!("legacy:{}", snapshot.store_id.as_str()));
        let registration = optional_text(row, 4)?
            .map(|id| id.parse::<WorkspaceId>().map_err(|_| unsupported()))
            .transpose()?;
        let relative = optional_text(row, 5)?;
        // An inconsistent legacy relative path cannot prove the old root.
        // Group by the saved cwd as unverified; the execution binding remains
        // unchanged and still requires validation before use.
        let derived_root = registration
            .and_then(|_| relative.as_deref())
            .and_then(|relative| derive_remote_root(&cwd, relative).ok());
        let plan_registration = if registration.is_some() && derived_root.is_none() {
            None
        } else {
            registration
        };
        sessions.push(LegacySession {
            id,
            parent_id,
            cwd: PathBuf::from(cwd),
            machine_id,
            execution_workspace_id: plan_registration,
            derived_root,
        });
    }
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
    let plan = plan_local_workspaces(&sessions, &registrations).map_err(|_| unsupported())?;
    if plan.session_workspace_ids.len() != sessions.len() {
        return Err(unsupported());
    }
    let mut statements = vec![
        StatementSpec::new(
            GUARD_META_SQL,
            vec![
                Value::Text(snapshot.store_id.as_str().to_owned()),
                Value::Text(snapshot.contract.clone()),
            ],
        ),
        StatementSpec::new(
            GUARD_OBJECT_COUNT_SQL,
            vec![Value::Integer(results[3].len() as i64)],
        ),
    ];
    for object in &results[3] {
        statements.push(StatementSpec::new(GUARD_OBJECT_SQL, object.clone()));
    }
    statements.push(StatementSpec::new(
        GUARD_COUNT_SQL,
        vec![Value::Integer(sessions.len() as i64)],
    ));
    for row in &results[0] {
        statements.push(StatementSpec::new(GUARD_ROW_SQL, row.clone()));
    }
    statements.push(StatementSpec::bare(
        "ALTER TABLE workspaces RENAME TO legacy_execution_registrations",
    ));
    statements.push(StatementSpec::bare(canonical::CREATE_V2_MACHINES_TABLE_SQL));
    statements.push(StatementSpec::bare(
        canonical::CREATE_V2_WORKSPACES_TABLE_SQL,
    ));
    let machine_ids: BTreeSet<_> = plan
        .workspaces
        .iter()
        .map(|workspace| workspace.machine_id.as_str())
        .collect();
    for machine_id in machine_ids {
        let known =
            uuid::Uuid::parse_str(machine_id).is_ok_and(|parsed| parsed.to_string() == machine_id);
        statements.push(StatementSpec::new(
            INSERT_MACHINE_SQL,
            vec![
                Value::Text(machine_id.to_owned()),
                Value::Text(if known { "我的电脑" } else { "旧机器" }.to_owned()),
                Value::Text(if known { "known" } else { "legacy_unknown" }.to_owned()),
            ],
        ));
    }
    for workspace in &plan.workspaces {
        let path = workspace.path.to_str().ok_or_else(unsupported)?;
        let source = match workspace.path_source {
            WorkspacePathSource::Discovered => "discovered",
            WorkspacePathSource::DerivedLegacy => "derived_legacy",
            WorkspacePathSource::Unverified => "unverified",
        };
        statements.push(StatementSpec::new(
            INSERT_WORKSPACE_SQL,
            vec![
                Value::Text(workspace.id.to_string()),
                Value::Text(workspace.machine_id.clone()),
                Value::Text(path.to_owned()),
                Value::Text(source.to_owned()),
            ],
        ));
    }
    statements.push(StatementSpec::bare(
        canonical::CREATE_V2_TEMP_THREADS_TABLE_SQL,
    ));
    for session in &sessions {
        let workspace_id = plan
            .session_workspace_ids
            .get(&session.id)
            .ok_or_else(unsupported)?;
        statements.push(StatementSpec::new(
            COPY_THREAD_SQL,
            vec![
                Value::Text(session.id.clone()),
                Value::Text(workspace_id.to_string()),
            ],
        ));
    }
    statements.push(StatementSpec::new(
        GUARD_TARGET_COUNT_SQL,
        vec![Value::Integer(sessions.len() as i64)],
    ));
    statements.push(StatementSpec::bare("DROP TABLE threads"));
    statements.push(StatementSpec::bare(
        "ALTER TABLE threads_v12 RENAME TO threads",
    ));
    statements.push(StatementSpec::bare(canonical::CREATE_INDEXES[3]));
    statements.push(StatementSpec::bare(canonical::CREATE_V2_INDEXES[4]));
    statements.push(StatementSpec::bare(
        "ALTER TABLE session_bindings ADD COLUMN discovery_snapshot TEXT",
    ));
    statements.push(StatementSpec::bare("ALTER TABLE session_bindings ADD COLUMN evidence_origin TEXT NOT NULL DEFAULT 'legacy_missing'"));
    statements.push(StatementSpec::bare("UPDATE session_bindings SET discovery_snapshot = (SELECT discovery FROM legacy_execution_registrations WHERE id = session_bindings.workspace_id)"));
    statements.push(StatementSpec::bare("UPDATE session_bindings SET evidence_origin = 'legacy_last_observation' WHERE discovery_snapshot IS NOT NULL"));
    statements.push(StatementSpec::bare("DROP TABLE session_environments"));
    statements.push(StatementSpec::bare("DROP TABLE mcp_oauth_credentials"));
    statements.push(StatementSpec::bare(
        canonical::CREATE_V2_OAUTH_CREDENTIALS_TABLE_SQL,
    ));
    statements.push(StatementSpec::new(
        UPDATE_META_SQL,
        vec![
            Value::Text("peri.session.store/v3".to_owned()),
            Value::Text(snapshot.store_id.as_str().to_owned()),
            Value::Text(snapshot.contract.clone()),
        ],
    ));
    store.apply_schema_upgrade(statements).await
}
