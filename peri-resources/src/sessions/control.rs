use peri_acp_types::session_resources::{
    ControlAction, ControlCommand, ControlDecision, ControlReceipt, ControlResolution,
    ControlState, SessionResourceError, SessionResourceResult,
};
use serde::{de::DeserializeOwned, Serialize};

use super::failure::corrupt;

pub(super) const CREATE_STATE: &str = "CREATE TABLE IF NOT EXISTS session_control_state (
    session_id TEXT PRIMARY KEY NOT NULL,
    state_json TEXT NOT NULL
)";
pub(super) const CREATE_RECEIPTS: &str = "CREATE TABLE IF NOT EXISTS session_control_receipts (
    command_id TEXT PRIMARY KEY NOT NULL,
    session_id TEXT NOT NULL,
    digest TEXT NOT NULL,
    resolution_json TEXT NOT NULL
)";
pub(super) const READ_STATE: &str =
    "SELECT state_json FROM session_control_state WHERE session_id = ?1";
pub(super) const READ_RECEIPT: &str =
    "SELECT digest, resolution_json FROM session_control_receipts WHERE command_id = ?1";
pub(super) const INSERT_STATE: &str = "INSERT OR IGNORE INTO session_control_state(session_id, state_json) SELECT ?1, ?2 FROM threads WHERE id = ?1";
pub(super) const GUARD_STATE: &str = "INSERT INTO session_control_state(session_id, state_json) SELECT NULL, NULL WHERE (?3 = 0 AND NOT EXISTS (SELECT 1 FROM threads WHERE id = ?1)) OR NOT EXISTS (SELECT 1 FROM session_control_state WHERE session_id = ?1 AND state_json = ?2) OR EXISTS(WITH RECURSIVE scope(id) AS (SELECT ?1 UNION ALL SELECT threads.id FROM threads JOIN scope ON threads.parent_thread_id=scope.id) SELECT 1 FROM session_work_commands WHERE session_id IN (SELECT id FROM scope) AND reconciled=0)";
pub(super) const UPDATE_STATE: &str =
    "UPDATE session_control_state SET state_json = ?2 WHERE session_id = ?1";
pub(super) const INSERT_RECEIPT: &str = "INSERT INTO session_control_receipts(command_id, session_id, digest, resolution_json) VALUES (?1, ?2, ?3, ?4)";
pub(super) const MARK_CLOSING: &str =
    "INSERT OR IGNORE INTO session_close_intents(thread_id, requested_at) VALUES (?1, ?2)";
pub(super) const CLEAR_CLOSING: &str = "DELETE FROM session_close_intents WHERE thread_id = ?1";
pub(super) const SEED_STATE: &str = "INSERT OR IGNORE INTO session_control_state(session_id, state_json)
    SELECT id, CASE WHEN EXISTS (SELECT 1 FROM session_close_intents WHERE thread_id = threads.id)
    THEN '{\"lifecycle\":1,\"revision\":0,\"controlGeneration\":0,\"status\":\"closing\",\"attempt\":null}'
    ELSE '{\"lifecycle\":1,\"revision\":0,\"controlGeneration\":0,\"status\":\"active\",\"attempt\":null}' END FROM threads";

pub(super) fn closing_projection(
    command: &ControlCommand,
    receipt: &ControlReceipt,
) -> Option<bool> {
    if receipt.decision != ControlDecision::Accepted {
        return None;
    }
    match command.action {
        ControlAction::Close => Some(true),
        ControlAction::FinishClose | ControlAction::Reopen => Some(false),
        _ => None,
    }
}

pub(super) fn encode(value: &impl Serialize) -> SessionResourceResult<String> {
    serde_json::to_string(value).map_err(|_| corrupt("control record is not serializable"))
}

pub(super) fn decode<Value: DeserializeOwned>(json: &str) -> SessionResourceResult<Value> {
    serde_json::from_str(json).map_err(|_| corrupt("control record is not readable"))
}

pub(super) fn replay(
    command: &ControlCommand,
    digest: &str,
    json: &str,
) -> SessionResourceResult<ControlResolution> {
    if command.digest()? != digest {
        return Err(SessionResourceError::conflict(
            "control command identity conflicts",
        ));
    }
    let resolution: ControlResolution = decode(json)?;
    if let ControlResolution::Applied { receipt } = &resolution {
        if receipt.session_id != command.session_id || receipt.command_id != command.command_id {
            return Err(corrupt("control receipt identity conflicts"));
        }
    }
    Ok(resolution)
}

pub(super) fn receipt(resolution: ControlResolution) -> SessionResourceResult<ControlReceipt> {
    match resolution {
        ControlResolution::Applied { receipt } => Ok(receipt),
        ControlResolution::NotApplied => Err(SessionResourceError::conflict(
            "control command was finalized without applying",
        )),
        ControlResolution::Unknown => {
            Err(corrupt("unknown control resolution cannot be persisted"))
        }
    }
}

pub(super) fn state(json: Option<&str>) -> SessionResourceResult<ControlState> {
    json.map(decode)
        .unwrap_or_else(|| Ok(ControlState::default()))
}
