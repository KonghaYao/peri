use peri_acp_types::session_resources::work::{
    LegacyEvidence, PayloadRef, RecoveryDescriptor, ResourceOwnerBinding, SessionWorkHead,
    WorkAction, WorkCommand, WorkLimits,
};
use peri_acp_types::session_resources::SessionResourceResult;
use serde::{Deserialize, Serialize};

use super::{payload, schema, SqlParam, SqlStatement};
use crate::sessions::{failure::corrupt, work::encode};

#[path = "migration_classification.rs"]
mod classification;
pub(crate) use classification::{classify_legacy, legacy_command_unresolved};

pub(crate) const LEGACY_EVENT_SCOPE: &str = "migration:legacy-events";

pub(crate) fn archive_scope_plan() -> Vec<SqlStatement> {
    vec![SqlStatement::new("INSERT INTO session_control_state(session_id,state_json) VALUES (?1,'{\"lifecycle\":1,\"revision\":0,\"controlGeneration\":0,\"status\":\"closed\",\"attempt\":null}') ON CONFLICT(session_id) DO NOTHING",vec![SqlParam::Text(LEGACY_EVENT_SCOPE.into())])]
}

pub(crate) fn import_head(
    session_id: &str,
    lifecycle: u64,
    json: &str,
    unresolved: bool,
) -> SessionResourceResult<Vec<SqlStatement>> {
    let source: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let limits = source
        .get("limits")
        .cloned()
        .map(serde_json::from_value::<WorkLimits>)
        .transpose()
        .map_err(|_| corrupt("legacy work limits cannot be mapped"))?
        .unwrap_or_default();
    let owner = source
        .get("resourceOwners")
        .and_then(|owners| owners.get(lifecycle.to_string()));
    let child_metadata = source
        .get("childResumeMetadata")
        .and_then(|metadata| metadata.get(lifecycle.to_string()))
        .and_then(serde_json::Value::as_str);
    let descriptor_id = if owner.is_some() || child_metadata.is_some() {
        Some(format!("legacy-recovery:{lifecycle}"))
    } else {
        None
    };
    let head = SessionWorkHead {
        lifecycle,
        change_seq: source
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        next_delivery_seq: source
            .get("nextAdmissionSequence")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1),
        limits,
        legacy_unknown: u64::from(unresolved),
        recovery_descriptor_id: descriptor_id,
        ..SessionWorkHead::default()
    };
    let mut statements=vec![SqlStatement::new("INSERT INTO session_work_head(session_id,lifecycle,format_version,revision,next_delivery_seq,record_json) VALUES (?1,?2,1,?3,?4,?5) ON CONFLICT(session_id,lifecycle) DO UPDATE SET revision=excluded.revision,next_delivery_seq=excluded.next_delivery_seq,record_json=json_set(excluded.record_json,'$.legacyUnknown',MAX(COALESCE(json_extract(session_work_head.record_json,'$.legacyUnknown'),0),?6))",vec![SqlParam::Text(session_id.into()),payload::integer(lifecycle)?,payload::integer(head.change_seq)?,payload::integer(head.next_delivery_seq)?,SqlParam::Text(encode(&head)?),SqlParam::Integer(i64::from(unresolved))])];
    let mut lifecycles = std::collections::BTreeSet::new();
    for field in ["resourceOwners", "childResumeMetadata"] {
        if let Some(records) = source.get(field).and_then(serde_json::Value::as_object) {
            for key in records.keys() {
                lifecycles.insert(
                    key.parse::<u64>()
                        .map_err(|_| corrupt("legacy recovery lifecycle is invalid"))?,
                );
            }
        }
    }
    for recovered_lifecycle in lifecycles {
        let owners = source
            .get("resourceOwners")
            .and_then(|owners| owners.get(recovered_lifecycle.to_string()))
            .cloned()
            .map(serde_json::from_value::<ResourceOwnerBinding>)
            .transpose()
            .map_err(|_| corrupt("legacy owner metadata cannot be mapped"))?;
        let metadata = source
            .get("childResumeMetadata")
            .and_then(|metadata| metadata.get(recovered_lifecycle.to_string()))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let descriptor = RecoveryDescriptor {
            descriptor_id: format!("legacy-recovery:{recovered_lifecycle}"),
            recipient_lifecycle: recovered_lifecycle,
            revision: 0,
            resource_owners: owners,
            child_resume_metadata_json: metadata,
        };
        let command = WorkCommand {
            session_id: session_id.into(),
            recipient_lifecycle: recovered_lifecycle,
            mutation_id: "migration-recovery".into(),
            action: WorkAction::QuarantineLegacy {
                expected_revision: 0,
                record_id: descriptor.descriptor_id.clone(),
                evidence: "immutable stopped-writer migration".into(),
            },
        };
        super::plan::auxiliary(
            &command,
            "recoveryDescriptor",
            &descriptor.descriptor_id,
            recovered_lifecycle,
            &descriptor,
            None,
            "0",
            &mut statements,
        )?;
    }
    Ok(statements)
}

#[derive(Clone, Debug)]
pub struct StoppedWriterApproval {
    pub source_version: i64,
    pub writers_stopped: bool,
    pub backup_verified: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkMigrationReport {
    pub sessions: u64,
    pub messages: u64,
    pub legacy_unknown_sessions: u64,
    pub retained_evidence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LegacyUnknownEvidence {
    pub format_version: u32,
    pub reason: String,
    pub original: PayloadRef,
    pub unresolved: bool,
}

pub(crate) fn initialization_plan(
    approval: &StoppedWriterApproval,
) -> SessionResourceResult<Vec<SqlStatement>> {
    if approval.source_version != 17 || !approval.writers_stopped || !approval.backup_verified {
        return Err(corrupt(
            "offline migration requires schema 17, verified backup and stopped writers",
        ));
    }
    let mut statements = vec![SqlStatement::bare(
        "SELECT 1 FROM session_work_state LIMIT 1",
    )];
    statements.extend(
        schema::CREATE_TABLES
            .iter()
            .copied()
            .map(SqlStatement::bare),
    );
    for sql in [
        "ALTER TABLE session_work_commands ADD COLUMN command_scope TEXT",
        "ALTER TABLE session_work_commands ADD COLUMN command_payload_id TEXT",
        "ALTER TABLE session_work_commands ADD COLUMN kind TEXT NOT NULL DEFAULT 'mutation'",
        "ALTER TABLE session_work_commands ADD COLUMN subject_id TEXT",
        "ALTER TABLE session_work_commands ADD COLUMN lifecycle INTEGER CHECK(lifecycle>=0)",
        "ALTER TABLE session_work_commands ADD COLUMN record_revision INTEGER CHECK(record_revision>=0)",
        "ALTER TABLE session_work_commands ADD COLUMN guard_token TEXT",
        "ALTER TABLE session_work_commands ADD COLUMN record_json TEXT",
    ] {
        statements.push(SqlStatement::bare(sql));
    }
    statements.extend(
        schema::CREATE_INDEXES
            .iter()
            .copied()
            .map(SqlStatement::bare),
    );
    Ok(statements)
}

pub(crate) fn import_blob(
    session_id: &str,
    lifecycle: u64,
    source_kind: &str,
    source_id: &str,
    bytes: &[u8],
    unresolved: bool,
) -> SessionResourceResult<Vec<SqlStatement>> {
    if session_id.is_empty()
        || lifecycle == 0
        || !matches!(source_kind, "work" | "command" | "receipt" | "event")
    {
        return Err(corrupt("invalid legacy evidence identity"));
    }
    let (original, mut statements) = payload::prepare_bytes(session_id, "legacyOriginal", bytes)?;
    let evidence=LegacyUnknownEvidence {format_version:1,reason:if unresolved {"legacy responsibility cannot be proven by the new execution protocol; explicit reconciliation is required"} else {"immutable pre-cutover evidence; proven terminal or non-executing history"}.into(),original,unresolved};
    let record = LegacyEvidence {
        record_id: format!("legacy:{source_kind}:{source_id}"),
        evidence: encode(&evidence)?,
    };
    let (reference, evidence_plan) =
        payload::prepare_bytes(session_id, "legacy", encode(&record)?.as_bytes())?;
    statements.extend(evidence_plan);
    let identity = format!(
        "record:{}",
        encode(&(session_id, lifecycle, "legacy", record.record_id.as_str()))?
    );
    let params = vec![
        SqlParam::Text(identity),
        SqlParam::Text(session_id.into()),
        SqlParam::Text(reference.sha256.clone()),
        SqlParam::Text(encode(&reference)?),
        SqlParam::Text(reference.storage_scope),
        SqlParam::Text(reference.payload_id),
        SqlParam::Text(record.record_id),
        payload::integer(lifecycle)?,
    ];
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id,session_id,digest,command_json,command_scope,command_payload_id,kind,subject_id,lifecycle,guard_token,record_json,reconciled) VALUES (?1,?2,?3,?4,?5,?6,'legacy',?7,?8,'0',?4,1)",params));
    if source_kind != "event" {
        let head = SessionWorkHead {
            lifecycle,
            legacy_unknown: u64::from(unresolved),
            ..SessionWorkHead::default()
        };
        statements.push(SqlStatement::new("INSERT INTO session_work_head(session_id,lifecycle,format_version,revision,next_delivery_seq,record_json) VALUES (?1,?2,1,0,1,?3) ON CONFLICT(session_id,lifecycle) DO UPDATE SET record_json=json_set(session_work_head.record_json,'$.legacyUnknown',MAX(COALESCE(json_extract(session_work_head.record_json,'$.legacyUnknown'),0),?4))",vec![SqlParam::Text(session_id.into()),payload::integer(lifecycle)?,SqlParam::Text(encode(&head)?),SqlParam::Integer(i64::from(unresolved))]));
    }
    Ok(statements)
}

pub(crate) fn import_message(
    message_id: &str,
    session_id: &str,
    role: &str,
    sequence: u64,
    content: &str,
) -> SessionResourceResult<Vec<SqlStatement>> {
    let decoded = peri_acp_types::store::deserialize_persisted_payload(content)
        .map_err(|_| corrupt("legacy canonical message is not readable"))?;
    if decoded.id().as_uuid().to_string() != message_id {
        return Err(corrupt("legacy canonical message identity conflicts"));
    }
    let actual_role = match &decoded {
        peri_acp_types::store::PersistedPayload::SystemReminder { .. } => "system_reminder",
        peri_acp_types::store::PersistedPayload::Message(message) => match message {
            peri_acp_types::messages::BaseMessage::Human { .. } => "user",
            peri_acp_types::messages::BaseMessage::Ai { .. } => "assistant",
            peri_acp_types::messages::BaseMessage::Tool { .. } => "tool",
            peri_acp_types::messages::BaseMessage::System { .. } => "system",
        },
    };
    if actual_role != role {
        return Err(corrupt("legacy canonical message role conflicts"));
    }
    let (reference, mut statements) =
        payload::prepare_bytes(session_id, "message", content.as_bytes())?;
    statements.push(SqlStatement::checked("UPDATE messages SET content_ref=?4,transcript_seq=?5 WHERE message_id=?1 AND thread_id=?2 AND role=?3 AND content=?6",vec![SqlParam::Text(message_id.into()),SqlParam::Text(session_id.into()),SqlParam::Text(role.into()),SqlParam::Text(encode(&reference)?),payload::integer(sequence)?,SqlParam::Text(content.into())],1));
    Ok(statements)
}

pub(crate) fn message_columns_plan() -> Vec<SqlStatement> {
    vec![
        SqlStatement::bare("ALTER TABLE messages ADD COLUMN content_ref TEXT"),
        SqlStatement::bare(
            "ALTER TABLE messages ADD COLUMN transcript_seq INTEGER CHECK(transcript_seq>=0)",
        ),
    ]
}

pub(crate) fn finalize_plan() -> Vec<SqlStatement> {
    vec![
        SqlStatement::bare("INSERT INTO session_payloads(storage_scope) SELECT NULL WHERE EXISTS(SELECT 1 FROM session_work_state legacy WHERE NOT EXISTS(SELECT 1 FROM session_payloads p WHERE p.storage_scope=legacy.session_id AND p.kind='legacyOriginal' AND p.bytes=CAST(legacy.state_json AS BLOB)))"),
        SqlStatement::bare("INSERT INTO session_payloads(storage_scope) SELECT NULL WHERE EXISTS(SELECT 1 FROM session_work_commands legacy WHERE legacy.kind='mutation' AND NOT EXISTS(SELECT 1 FROM session_payloads p WHERE p.storage_scope=legacy.session_id AND p.kind='legacyOriginal' AND p.bytes=CAST(legacy.command_json AS BLOB)))"),
        SqlStatement::bare("INSERT INTO session_payloads(storage_scope) SELECT NULL WHERE EXISTS(SELECT 1 FROM session_work_receipts legacy WHERE NOT EXISTS(SELECT 1 FROM session_payloads p WHERE p.storage_scope=legacy.session_id AND p.kind='legacyOriginal' AND p.bytes=CAST(legacy.resolution_json AS BLOB)))"),
        SqlStatement::bare("INSERT INTO session_payloads(storage_scope) SELECT NULL WHERE EXISTS(SELECT 1 FROM session_work_events legacy WHERE NOT EXISTS(SELECT 1 FROM session_payloads p WHERE p.kind='legacyOriginal' AND p.bytes=CAST(legacy.event_json AS BLOB)))"),
        SqlStatement::bare("INSERT INTO session_payloads(storage_scope) SELECT NULL WHERE EXISTS(SELECT 1 FROM messages WHERE content_ref IS NULL OR transcript_seq IS NULL)"),
        SqlStatement::bare("INSERT INTO session_control_state(session_id,state_json) SELECT h.session_id,json_object('lifecycle',MAX(h.lifecycle),'revision',0,'controlGeneration',0,'status',CASE WHEN EXISTS(SELECT 1 FROM threads WHERE id=h.session_id) THEN 'active' ELSE 'closed' END,'attempt',NULL) FROM session_work_head h GROUP BY h.session_id ON CONFLICT(session_id) DO NOTHING"),
        SqlStatement::bare("UPDATE session_work_commands SET kind='legacyCommand',reconciled=1 WHERE kind='mutation'"),
        SqlStatement::bare("DELETE FROM session_work_events"),
        SqlStatement::bare("DROP TABLE session_work_state"),
        SqlStatement::bare("ALTER TABLE messages DROP COLUMN content"),
        SqlStatement::bare("CREATE UNIQUE INDEX idx_messages_transcript ON messages(thread_id,transcript_seq)"),
    ]
}
