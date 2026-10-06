use peri_acp_types::session_resources::work::{
    WorkCommand, WorkReceipt, WorkReduction, WorkResolution, WorkState,
};
use peri_acp_types::session_resources::{
    ControlState, SessionResourceError, SessionResourceResult,
};
use serde::{de::DeserializeOwned, Serialize};

use super::failure::corrupt;

#[path = "work/effects.rs"]
mod effects;
pub(super) use effects::mutation_effects;

pub(super) const CREATE_STATE: &str = "CREATE TABLE IF NOT EXISTS session_work_state (session_id TEXT PRIMARY KEY NOT NULL, state_json TEXT NOT NULL)";
pub(super) const CREATE_EVENTS: &str = "CREATE TABLE IF NOT EXISTS session_work_events (event_key TEXT PRIMARY KEY NOT NULL, event_json TEXT NOT NULL)";
pub(super) const CREATE_RECEIPTS: &str = "CREATE TABLE IF NOT EXISTS session_work_receipts (mutation_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL, digest TEXT NOT NULL, resolution_json TEXT NOT NULL)";
pub(super) const CREATE_COMMANDS: &str = "CREATE TABLE IF NOT EXISTS session_work_commands (mutation_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL, digest TEXT NOT NULL, command_json TEXT NOT NULL, reconciled INTEGER NOT NULL DEFAULT 0 CHECK(reconciled IN (0,1)))";
pub(super) const INSERT_COMMAND: &str = "INSERT OR IGNORE INTO session_work_commands(mutation_id,session_id,digest,command_json) VALUES (?1,?2,?3,?4)";
pub(super) const GUARD_COMMAND: &str = "INSERT INTO session_work_commands(mutation_id,session_id,digest,command_json) SELECT NULL,NULL,NULL,NULL WHERE NOT EXISTS (SELECT 1 FROM session_work_commands WHERE mutation_id=?1 AND session_id=?2 AND digest=?3 AND command_json=?4) OR EXISTS (SELECT 1 FROM session_work_commands WHERE session_id=?2 AND mutation_id<>?1 AND reconciled=0)";
pub(super) const READ_COMMAND: &str =
    "SELECT command_json FROM session_work_commands WHERE mutation_id=?1";
pub(super) const READ_OWNED_COMMAND: &str = "SELECT c.command_json,c.digest,r.resolution_json,c.reconciled FROM session_work_commands c LEFT JOIN session_work_receipts r ON r.mutation_id=c.mutation_id AND r.session_id=c.session_id AND r.digest=c.digest WHERE c.mutation_id=?1 AND c.session_id=?2";
pub(super) const READ_PENDING: &str = "WITH RECURSIVE scope(id) AS (SELECT ?1 UNION ALL SELECT threads.id FROM threads JOIN scope ON threads.parent_thread_id=scope.id) SELECT command_json FROM session_work_commands WHERE session_id IN (SELECT id FROM scope) AND reconciled=0 ORDER BY mutation_id";
pub(super) const ACK_COMMAND: &str = "UPDATE session_work_commands SET reconciled=1 WHERE mutation_id=?1 AND digest=?2 AND EXISTS(SELECT 1 FROM session_work_receipts WHERE mutation_id=?1 AND digest=?2)";

pub(super) fn command_effects(command: &WorkCommand) -> SessionResourceResult<Vec<WorkEffect>> {
    let params = [
        command.mutation_id.clone(),
        command.session_id.clone(),
        command.digest()?,
        encode(command)?,
    ];
    Ok(vec![
        WorkEffect::texts(INSERT_COMMAND, params.clone()),
        WorkEffect::texts(GUARD_COMMAND, params),
    ])
}

pub(super) fn original_command(json: &str) -> SessionResourceResult<WorkCommand> {
    let command: WorkCommand = decode(json)?;
    command.digest()?;
    Ok(command)
}

pub(super) fn owned_command(
    json: &str,
    digest: &str,
    resolution: Option<&str>,
    reconciled: bool,
) -> SessionResourceResult<peri_acp_types::session_resources::work::OwnedWorkCommand> {
    let command = original_command(json)?;
    if command.digest()? != digest {
        return Err(corrupt("owned command digest conflicts"));
    }
    let resolution = resolution
        .map(|resolution| replay(&command, digest, resolution))
        .transpose()?;
    if reconciled && resolution.is_none() {
        return Err(corrupt("reconciled command lacks original receipt"));
    }
    Ok(peri_acp_types::session_resources::work::OwnedWorkCommand {
        command,
        resolution,
        pending: !reconciled,
    })
}
pub(super) const READ_STATE: &str =
    "SELECT state_json FROM session_work_state WHERE session_id = ?1";
pub(super) const READ_RECEIPT: &str =
    "SELECT digest, resolution_json FROM session_work_receipts WHERE mutation_id = ?1";
pub(super) const INSERT_STATE: &str =
    "INSERT OR IGNORE INTO session_work_state(session_id,state_json) VALUES (?1,?2)";
pub(super) const GUARD_STATE: &str = "INSERT INTO session_work_state(session_id,state_json) SELECT NULL,NULL WHERE NOT EXISTS (SELECT 1 FROM session_work_state WHERE session_id=?1 AND state_json=?2) OR COALESCE((SELECT state_json FROM session_control_state WHERE session_id=?1),?3) <> ?4";
pub(super) const UPDATE_STATE: &str =
    "UPDATE session_work_state SET state_json=?2 WHERE session_id=?1";
pub(super) const INSERT_RECEIPT: &str = "INSERT INTO session_work_receipts(mutation_id,session_id,digest,resolution_json) VALUES (?1,?2,?3,?4)";

pub(super) struct WorkEffect {
    pub sql: &'static str,
    pub params: Vec<String>,
}
impl WorkEffect {
    pub(super) fn texts(sql: &'static str, params: impl IntoIterator<Item = String>) -> Self {
        Self {
            sql,
            params: params.into_iter().collect(),
        }
    }
}

pub(super) fn encode(value: &impl Serialize) -> SessionResourceResult<String> {
    serde_json::to_string(value).map_err(|_| corrupt("work record is not serializable"))
}
pub(super) fn decode<Value: DeserializeOwned>(value: &str) -> SessionResourceResult<Value> {
    serde_json::from_str(value).map_err(|_| corrupt("work record is not readable"))
}
pub(super) fn state(
    json: Option<&str>,
    has_legacy_history: bool,
) -> SessionResourceResult<WorkState> {
    if let Some(json) = json {
        return decode(json);
    }
    let mut state = WorkState::default();
    if has_legacy_history {
        state.legacy_unknown.insert("pre-ledger-history".into(), "canonical history has no durable processing checkpoint; explicit reconciliation required".into());
    }
    Ok(state)
}
pub(super) fn replay(
    command: &WorkCommand,
    digest: &str,
    json: &str,
) -> SessionResourceResult<WorkResolution> {
    if command.digest()? != digest {
        return Err(SessionResourceError::conflict(
            "work mutation identity conflicts",
        ));
    }
    let resolution: WorkResolution = decode(json)?;
    if let WorkResolution::Applied { receipt } = &resolution {
        if receipt.session_id != command.session_id {
            return Err(corrupt("work receipt belongs to another session"));
        }
    }
    Ok(resolution)
}
pub(super) fn receipt(resolution: WorkResolution) -> SessionResourceResult<WorkReceipt> {
    match resolution {
        WorkResolution::Applied { receipt } => Ok(receipt),
        WorkResolution::NotApplied => Err(SessionResourceError::conflict(
            "work mutation was finalized without applying",
        )),
        WorkResolution::Unknown => Err(corrupt("unknown work resolution cannot be persisted")),
    }
}

pub(super) fn initial_state_json() -> SessionResourceResult<String> {
    encode(&WorkState::default())
}
pub(super) fn legacy_state_json() -> SessionResourceResult<String> {
    encode(&state(None, true)?)
}

pub(super) const SEED_STATE: &str = "INSERT OR IGNORE INTO session_work_state(session_id,state_json) SELECT id, CASE WHEN EXISTS (SELECT 1 FROM messages WHERE thread_id=threads.id) THEN ?1 ELSE ?2 END FROM threads";
pub(super) const QUARANTINE_UNOWNED_COMMANDS: &str = "UPDATE session_work_state SET state_json=json_set(state_json,'$.legacyUnknown.unowned-command-history','pre-journal work receipts lack original commands; explicit reconciliation required') WHERE EXISTS(SELECT 1 FROM session_work_receipts WHERE session_id=session_work_state.session_id AND NOT EXISTS(SELECT 1 FROM session_work_commands WHERE mutation_id=session_work_receipts.mutation_id))";
