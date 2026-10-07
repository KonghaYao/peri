use peri_acp_types::session_resources::work::{
    DeliveryRecord, ResourceOwnerBinding, ResourceOwnerFacts, WorkAction, WorkCommand,
    WorkDeliveryQuery, WorkReceipt, WorkReduction, WorkResolution, WorkState,
};
use peri_acp_types::session_resources::{
    ControlState, SessionResourceError, SessionResourceResult,
};
use serde::{de::DeserializeOwned, Serialize};

use super::failure::corrupt;

#[path = "work/availability.rs"]
mod availability;
pub(super) use availability::{availability, READ_AVAILABILITY};

#[path = "work/effects.rs"]
mod effects;
pub(super) use effects::mutation_effects;

#[path = "work/diagnostics.rs"]
mod diagnostics;
pub(super) use diagnostics::WorkPhase;

pub(super) const CREATE_STATE: &str = "CREATE TABLE IF NOT EXISTS session_work_state (session_id TEXT PRIMARY KEY NOT NULL, state_json TEXT NOT NULL)";
pub(super) const READ_REVISION: &str = "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1), EXISTS(SELECT 1 FROM session_control_state WHERE session_id=?1), state.state_json IS NOT NULL, state.state_json -> '$.revision' FROM (SELECT 1) LEFT JOIN session_work_state AS state ON state.session_id=?1";
pub(super) const READ_RESOURCE_OWNER_FACTS: &str = r#"
SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1), control.state_json,
       1, state.state_json -> '$.revision',
       state.state_json -> ('$.resourceOwners.' || COALESCE(json_extract(control.state_json,'$.lifecycle'),1)),
       state.state_json -> ('$.resourceOwners.' || ?2),
       state.state_json -> ('$.childResumeMetadata.' || COALESCE(json_extract(control.state_json,'$.lifecycle'),1)),
       state.state_json -> ('$.childResumeMetadata.' || ?2),
       json_type(state.state_json,'$.resourceOwners'),
       json_type(state.state_json,'$.childResumeMetadata')
FROM session_work_state AS state
LEFT JOIN session_control_state AS control ON control.session_id=state.session_id
WHERE state.session_id=?1
UNION ALL
SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1), control.state_json,
       0, NULL, NULL, NULL, NULL, NULL, NULL, NULL
FROM (SELECT 1)
LEFT JOIN session_control_state AS control ON control.session_id=?1
WHERE NOT EXISTS(SELECT 1 FROM session_work_state WHERE session_id=?1)
"#;

pub(super) struct ResourceOwnerRow<'a> {
    pub session_exists: bool,
    pub control_json: Option<&'a str>,
    pub state_exists: bool,
    pub revision_json: Option<&'a str>,
    pub current_owner_json: Option<&'a str>,
    pub previous_owner_json: Option<&'a str>,
    pub current_child_json: Option<&'a str>,
    pub previous_child_json: Option<&'a str>,
    pub owners_type: Option<&'a str>,
    pub child_metadata_type: Option<&'a str>,
}

pub(super) fn resource_owner_facts(
    row: ResourceOwnerRow<'_>,
) -> SessionResourceResult<ResourceOwnerFacts> {
    let revision = revision(
        i64::from(row.session_exists),
        i64::from(row.control_json.is_some()),
        i64::from(row.state_exists),
        row.revision_json,
    )?;
    if row.state_exists
        && (row.owners_type != Some("object")
            || !matches!(row.child_metadata_type, None | Some("object")))
    {
        return Err(corrupt("resource owner state is not readable"));
    }
    Ok(ResourceOwnerFacts {
        control: crate::sessions::control::state(row.control_json)?,
        revision,
        current_owner: row
            .current_owner_json
            .map(decode::<ResourceOwnerBinding>)
            .transpose()?,
        previous_owner: row
            .previous_owner_json
            .map(decode::<ResourceOwnerBinding>)
            .transpose()?,
        current_child_metadata: row.current_child_json.map(decode::<String>).transpose()?,
        previous_child_metadata: row.previous_child_json.map(decode::<String>).transpose()?,
    })
}

pub(super) fn revision(
    session_exists: i64,
    control_exists: i64,
    state_exists: i64,
    revision_json: Option<&str>,
) -> SessionResourceResult<u64> {
    if session_exists == 0 && control_exists == 0 && state_exists == 0 {
        return Err(SessionResourceError::new(
            peri_acp_types::session_resources::SessionResourceErrorKind::NotFound,
        ));
    }
    if state_exists == 0 {
        return Ok(0);
    }
    revision_json
        .ok_or_else(|| corrupt("work revision is not readable"))
        .and_then(|value| {
            serde_json::from_str(value).map_err(|_| corrupt("work revision is not readable"))
        })
}
pub(super) const CREATE_EVENTS: &str = "CREATE TABLE IF NOT EXISTS session_work_events (event_key TEXT PRIMARY KEY NOT NULL, event_json TEXT NOT NULL)";
pub(super) const CREATE_RECEIPTS: &str = "CREATE TABLE IF NOT EXISTS session_work_receipts (mutation_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL, digest TEXT NOT NULL, resolution_json TEXT NOT NULL)";
pub(super) const CREATE_COMMANDS: &str = "CREATE TABLE IF NOT EXISTS session_work_commands (mutation_id TEXT PRIMARY KEY NOT NULL, session_id TEXT NOT NULL, digest TEXT NOT NULL, command_json TEXT NOT NULL, reconciled INTEGER NOT NULL DEFAULT 0 CHECK(reconciled IN (0,1)))";
pub(super) const INSERT_COMMAND: &str = "INSERT OR IGNORE INTO session_work_commands(mutation_id,session_id,digest,command_json) VALUES (?1,?2,?3,?4)";
pub(super) const GUARD_COMMAND: &str = "INSERT INTO session_work_commands(mutation_id,session_id,digest,command_json) SELECT NULL,NULL,NULL,NULL WHERE NOT EXISTS (SELECT 1 FROM session_work_commands WHERE mutation_id=?1 AND session_id=?2 AND digest=?3 AND command_json=?4) OR EXISTS (SELECT 1 FROM session_work_commands NOT INDEXED WHERE reconciled=0 AND session_id=?2 AND mutation_id<>?1)";
pub(super) const READ_COMMAND: &str =
    "SELECT command_json FROM session_work_commands WHERE mutation_id=?1";
pub(super) const READ_OWNED_COMMAND: &str = "SELECT c.command_json,c.digest,r.resolution_json,c.reconciled FROM session_work_commands c LEFT JOIN session_work_receipts r ON r.mutation_id=c.mutation_id AND r.session_id=c.session_id AND r.digest=c.digest WHERE c.mutation_id=?1 AND c.session_id=?2";
pub(super) const READ_PENDING: &str = "WITH RECURSIVE scope(id) AS (SELECT ?1 UNION ALL SELECT threads.id FROM threads JOIN scope ON threads.parent_thread_id=scope.id) SELECT command_json FROM session_work_commands NOT INDEXED WHERE reconciled=0 AND session_id IN (SELECT id FROM scope) ORDER BY mutation_id";
pub(super) const HAS_PENDING: &str = "WITH RECURSIVE scope(id) AS (SELECT ?1 UNION ALL SELECT threads.id FROM threads JOIN scope ON threads.parent_thread_id=scope.id) SELECT EXISTS (SELECT 1 FROM session_work_commands NOT INDEXED WHERE reconciled=0 AND session_id IN (SELECT id FROM scope))";
pub(super) const ACK_COMMAND: &str = "UPDATE session_work_commands SET reconciled=1 WHERE mutation_id=?1 AND digest=?2 AND EXISTS(SELECT 1 FROM session_work_receipts WHERE mutation_id=?1 AND digest=?2)";

pub(super) fn command_effects(command: &WorkCommand) -> SessionResourceResult<Vec<WorkEffect>> {
    command_effects_with_digest(command, &command.digest()?)
}

pub(super) fn command_effects_with_digest(
    command: &WorkCommand,
    digest: &str,
) -> SessionResourceResult<Vec<WorkEffect>> {
    let params = [
        command.mutation_id.clone(),
        command.session_id.clone(),
        digest.to_owned(),
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
#[cfg(test)]
pub(super) const READ_STATE: &str =
    "SELECT state_json FROM session_work_state WHERE session_id = ?1";
pub(super) const READ_SNAPSHOT: &str = "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1),control.state_json,state.state_json,CASE WHEN state.state_json IS NULL THEN EXISTS(SELECT 1 FROM messages WHERE thread_id=?1) ELSE 0 END FROM (SELECT 1) LEFT JOIN session_control_state AS control ON control.session_id=?1 LEFT JOIN session_work_state AS state ON state.session_id=?1";
pub(super) const READ_DELIVERY: &str = "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1 UNION ALL SELECT 1 FROM session_control_state WHERE session_id=?1 UNION ALL SELECT 1 FROM session_work_state WHERE session_id=?1),entry.type,entry.value FROM (SELECT 1) LEFT JOIN session_work_state AS state ON state.session_id=?1 LEFT JOIN json_each(state.state_json,'$.deliveries') AS entry ON entry.key=?2";

pub(super) fn delivery(
    query: &WorkDeliveryQuery,
    kind: Option<&str>,
    json: Option<&str>,
) -> SessionResourceResult<Option<DeliveryRecord>> {
    match (kind, json) {
        (None, None) => Ok(None),
        (Some("object"), Some(json)) => {
            let record: DeliveryRecord = decode(json)?;
            if record.publication.delivery_id != query.delivery_id {
                return Err(corrupt("work delivery identity conflicts"));
            }
            Ok(Some(record))
        }
        _ => Err(corrupt("work delivery is not readable")),
    }
}
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

/// JSON to persist for the pre-command state, computed before the state is moved
/// into `reduce_work`: the raw column value when the session already has a state
/// row, otherwise the encoding of the decoded state.
pub(super) fn pre_state_json(
    state_json: Option<String>,
    current: &WorkState,
) -> SessionResourceResult<String> {
    match state_json {
        Some(json) => Ok(json),
        None => encode(current),
    }
}

/// Command recorded for an in-flight terminal obligation, present only when the
/// command acknowledges one. Read from the pre-command state, which `reduce_work`
/// consumes, so the caller must extract it first.
pub(super) fn terminal_parent_command(
    command: &WorkCommand,
    current: &WorkState,
) -> Option<WorkCommand> {
    match &command.action {
        WorkAction::AcknowledgeTerminalObligation { admission_id, .. } => {
            current.terminal_obligations.get(admission_id).cloned()
        }
        _ => None,
    }
}

pub(super) fn replay(
    command: &WorkCommand,
    digest: &str,
    json: &str,
) -> SessionResourceResult<WorkResolution> {
    replay_with_digest(command, &command.digest()?, digest, json)
}

pub(super) fn replay_with_digest(
    command: &WorkCommand,
    command_digest: &str,
    stored_digest: &str,
    json: &str,
) -> SessionResourceResult<WorkResolution> {
    if command_digest != stored_digest {
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
