use peri_acp_types::session_resources::work::{
    OwnedWorkCommand, WorkCommand, WorkReceipt, WorkResolution,
};
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceResult};
use serde::{de::DeserializeOwned, Serialize};

use super::{
    failure::corrupt,
    work_store::{payload, SqlParam, SqlStatement},
};

pub(super) const INSERT_COMMAND: &str = "INSERT OR IGNORE INTO session_work_commands(mutation_id,session_id,digest,command_json,command_scope,command_payload_id,lifecycle) VALUES (?1,?2,?3,?4,?5,?6,?7)";
pub(super) const GUARD_COMMAND: &str = "INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_work_commands WHERE mutation_id=?1 AND session_id=?2 AND digest=?3 AND command_json=?4 AND kind='mutation') OR EXISTS(SELECT 1 FROM session_work_commands WHERE session_id=?2 AND mutation_id<>?1 AND reconciled=0 AND kind='mutation')";
pub(super) const READ_COMMAND: &str = "SELECT CAST(p.bytes AS TEXT) FROM session_work_commands c JOIN session_payloads p ON p.storage_scope=c.command_scope AND p.payload_id=c.command_payload_id WHERE c.mutation_id=?1 AND c.kind='mutation'";
pub(super) const READ_OWNED_COMMAND: &str = "SELECT CAST(p.bytes AS TEXT),c.digest,r.resolution_json,c.reconciled FROM session_work_commands c JOIN session_payloads p ON p.storage_scope=c.command_scope AND p.payload_id=c.command_payload_id LEFT JOIN session_work_receipts r ON r.mutation_id=c.mutation_id AND r.session_id=c.session_id AND r.digest=c.digest WHERE c.mutation_id=?1 AND c.session_id=?2 AND c.kind='mutation'";
pub(super) const READ_PENDING: &str = "WITH RECURSIVE scope(id) AS (SELECT ?1 UNION ALL SELECT threads.id FROM threads JOIN scope ON threads.parent_thread_id=scope.id) SELECT CAST(p.bytes AS TEXT) FROM session_work_commands c JOIN session_payloads p ON p.storage_scope=c.command_scope AND p.payload_id=c.command_payload_id WHERE c.session_id IN (SELECT id FROM scope) AND c.reconciled=0 AND c.kind='mutation' ORDER BY c.mutation_id LIMIT 64";
pub(super) const HAS_PENDING: &str = "WITH RECURSIVE scope(id) AS (SELECT ?1 UNION ALL SELECT threads.id FROM threads JOIN scope ON threads.parent_thread_id=scope.id) SELECT EXISTS(SELECT 1 FROM session_work_commands WHERE session_id IN (SELECT id FROM scope) AND reconciled=0 AND kind='mutation')";
pub(super) const ACK_COMMAND: &str = "UPDATE session_work_commands SET reconciled=1 WHERE mutation_id=?1 AND digest=?2 AND kind='mutation' AND EXISTS(SELECT 1 FROM session_work_receipts WHERE mutation_id=?1 AND digest=?2)";
pub(super) const READ_RECEIPT: &str =
    "SELECT digest,resolution_json FROM session_work_receipts WHERE mutation_id=?1";
pub(super) const INSERT_RECEIPT: &str = "INSERT INTO session_work_receipts(mutation_id,session_id,digest,resolution_json) VALUES (?1,?2,?3,?4)";

pub(super) fn command_effects(command: &WorkCommand) -> SessionResourceResult<Vec<SqlStatement>> {
    if command.mutation_id.starts_with("record:") {
        return Err(SessionResourceError::conflict(
            "reserved work record identity",
        ));
    }
    let digest = command.digest()?;
    let (reference, mut statements) =
        payload::prepare_bytes(&command.session_id, "command", encode(command)?.as_bytes())?;
    let params = vec![
        SqlParam::Text(command.mutation_id.clone()),
        SqlParam::Text(command.session_id.clone()),
        SqlParam::Text(digest),
        SqlParam::Text(encode(&reference)?),
        SqlParam::Text(reference.storage_scope),
        SqlParam::Text(reference.payload_id),
        payload::integer(command.recipient_lifecycle)?,
    ];
    statements.push(SqlStatement::new(INSERT_COMMAND, params.clone()));
    statements.push(SqlStatement::new(
        GUARD_COMMAND,
        params.into_iter().take(4).collect(),
    ));
    Ok(statements)
}

pub(super) fn encode(value: &impl Serialize) -> SessionResourceResult<String> {
    serde_json::to_string(value).map_err(|error| {
        tracing::error!(%error,"work record serialization failed");
        corrupt("work record is not serializable")
    })
}

pub(super) fn decode<Value: DeserializeOwned>(value: &str) -> SessionResourceResult<Value> {
    serde_json::from_str(value).map_err(|error| {
        tracing::error!(%error,"work record decoding failed");
        corrupt("work record is not readable")
    })
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
) -> SessionResourceResult<OwnedWorkCommand> {
    let command = original_command(json)?;
    if command.digest()? != digest {
        return Err(corrupt("owned command digest conflicts"));
    }
    let resolution = resolution
        .map(|json| replay(&command, digest, json))
        .transpose()?;
    if reconciled && resolution.is_none() {
        return Err(corrupt("reconciled command lacks original receipt"));
    }
    Ok(OwnedWorkCommand {
        command,
        resolution,
        pending: !reconciled,
    })
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
        if receipt.session_id != command.session_id || receipt.mutation_id != command.mutation_id {
            return Err(corrupt("work receipt identity conflicts"));
        }
    }
    Ok(resolution)
}

pub(super) fn receipt(resolution: WorkResolution) -> SessionResourceResult<WorkReceipt> {
    match resolution {
        WorkResolution::Applied { receipt } => Ok(receipt),
        WorkResolution::NotApplied => Err(SessionResourceError::conflict(
            "work mutation was finally sealed not applied",
        )),
        WorkResolution::Unknown => Err(SessionResourceError::persistence_uncertain(None)),
    }
}
