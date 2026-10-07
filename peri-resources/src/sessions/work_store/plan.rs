use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::SessionResourceResult;
use serde::Serialize;

use super::{payload, SqlParam, SqlStatement};
use crate::sessions::{failure::corrupt, work::encode};

fn text(value: impl Into<String>) -> SqlParam {
    SqlParam::Text(value.into())
}
fn optional(value: &Option<String>) -> SqlParam {
    value.as_ref().map(text).unwrap_or(SqlParam::Null)
}
fn enum_text(value: &impl Serialize) -> SessionResourceResult<SqlParam> {
    let encoded = serde_json::to_value(value).map_err(|_| corrupt("invalid work enum"))?;
    encoded
        .as_str()
        .map(text)
        .ok_or_else(|| corrupt("work enum is not scalar"))
}

const GUARD_HEAD: &str = "INSERT INTO session_work_head(session_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_work_head WHERE session_id=?1 AND lifecycle=?2 AND revision=?3 AND format_version=1)";
const INSERT_HEAD: &str = "INSERT OR IGNORE INTO session_work_head(session_id,lifecycle,format_version,revision,next_delivery_seq,record_json) VALUES (?1,?2,1,?3,?4,?5)";
const GUARD_CONTROL: &str = "INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE COALESCE((SELECT json_extract(state_json,'$.lifecycle') FROM session_control_state WHERE session_id=?1),1)<>?2 OR COALESCE((SELECT json_extract(state_json,'$.revision') FROM session_control_state WHERE session_id=?1),0)<>?3 OR COALESCE((SELECT json_extract(state_json,'$.controlGeneration') FROM session_control_state WHERE session_id=?1),0)<>?4";
const GUARD_JOURNAL: &str = "INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_work_commands WHERE mutation_id=?1 AND session_id=?2 AND digest=?3 AND kind='mutation' AND reconciled=0) OR EXISTS(SELECT 1 FROM session_work_receipts WHERE mutation_id=?1) OR EXISTS(SELECT 1 FROM session_work_commands WHERE session_id=?2 AND mutation_id<>?1 AND reconciled=0 AND kind='mutation')";

pub(crate) fn sql_plan(
    command: &WorkCommand,
    facts: &WorkFacts,
    transition: &WorkTransition,
) -> SessionResourceResult<Vec<SqlStatement>> {
    if facts.session_id != command.session_id
        || transition.receipt.session_id != command.session_id
        || transition.receipt.mutation_id != command.mutation_id
    {
        return Err(corrupt("work plan identity conflicts"));
    }
    if transition.writes.len() > MAX_WORK_PAGE_SIZE as usize * 4 + 8 {
        return Err(corrupt("work write plan exceeds bound"));
    }
    let mut statements = vec![
        SqlStatement::new(
            GUARD_JOURNAL,
            vec![
                text(&command.mutation_id),
                text(&command.session_id),
                text(command.digest()?),
            ],
        ),
        SqlStatement::new(
            GUARD_CONTROL,
            vec![
                text(&command.session_id),
                payload::integer(facts.control.lifecycle)?,
                payload::integer(facts.control.revision)?,
                payload::integer(facts.control.control_generation)?,
            ],
        ),
        SqlStatement::new(
            INSERT_HEAD,
            vec![
                text(&command.session_id),
                payload::integer(facts.head.lifecycle)?,
                payload::integer(facts.head.change_seq)?,
                payload::integer(facts.head.next_delivery_seq)?,
                text(encode(&facts.head)?),
            ],
        ),
        SqlStatement::new(
            GUARD_HEAD,
            vec![
                text(&command.session_id),
                payload::integer(facts.head.lifecycle)?,
                payload::integer(facts.head.change_seq)?,
            ],
        ),
    ];
    if transition.receipt.decision == WorkDecision::Accepted {
        guard_command_payloads(command, &mut statements)?;
    }
    guard_read_rows(command, facts, &mut statements)?;
    for write in &transition.writes {
        append_write(command, write, &mut statements)?;
    }
    Ok(statements)
}

fn guard_command_payloads(
    command: &WorkCommand,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    match &command.action {
        WorkAction::PublishDelivery { delivery }
        | WorkAction::PublishTaskSettlement { delivery, .. } => {
            statements.push(canonical_guard(&delivery.event.content)?)
        }
        WorkAction::PublishStagedUserInputs { deliveries, .. } => {
            for delivery in deliveries {
                statements.push(canonical_guard(&delivery.event.content)?);
            }
        }
        WorkAction::CommitReasonResponseAndDispatchIntent { response, .. } => {
            statements.push(canonical_guard(response)?)
        }
        WorkAction::CommitAct { results, .. } => {
            for result in results {
                match &result.outcome {
                    InvocationOutcome::Completed { result }
                    | InvocationOutcome::Failed { result }
                    | InvocationOutcome::Cancelled { result, .. } => {
                        statements.push(canonical_guard(result)?)
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn canonical_guard(message: &WorkPayload) -> SessionResourceResult<SqlStatement> {
    message.validate()?;
    Ok(SqlStatement::new(
        "INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_payloads WHERE storage_scope=?1 AND payload_id=?2 AND json_valid(CAST(bytes AS TEXT)) AND json_extract(CAST(bytes AS TEXT),'$.version')=1 AND ((?4='system_reminder' AND json_extract(CAST(bytes AS TEXT),'$.type')='system_reminder' AND json_extract(CAST(bytes AS TEXT),'$.id')=?3) OR (?4<>'system_reminder' AND json_extract(CAST(bytes AS TEXT),'$.type')='message' AND json_extract(CAST(bytes AS TEXT),'$.message.id')=?3 AND json_extract(CAST(bytes AS TEXT),'$.message.role')=?4 AND (?4<>'tool' OR json_extract(CAST(bytes AS TEXT),'$.message.tool_call_id')=?5))))",
        vec![
            text(&message.content.storage_scope),
            text(&message.content.payload_id),
            text(message.message_id.as_uuid().to_string()),
            text(&message.role),
            optional(&message.tool_call_id),
        ],
    ))
}

fn guard_read_rows(
    command: &WorkCommand,
    facts: &WorkFacts,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    for record in &facts.deliveries {
        statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_deliveries WHERE delivery_id=?1 AND session_id=?2 AND lifecycle=?3 AND revision=?4)",vec![text(&record.delivery_id),text(&command.session_id),payload::integer(record.recipient_lifecycle)?,payload::integer(record.revision)?]));
    }
    for record in &facts.effects {
        statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_effects WHERE invocation_id=?1 AND session_id=?2 AND lifecycle=?3 AND revision=?4)",vec![text(&record.invocation_id),text(&command.session_id),payload::integer(record.recipient_lifecycle)?,payload::integer(record.revision)?]));
    }
    if let Some(record) = &facts.processing {
        statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_processing WHERE processing_id=?1 AND session_id=?2 AND lifecycle=?3 AND revision=?4)",vec![text(&record.processing_id),text(&command.session_id),payload::integer(record.recipient_lifecycle)?,payload::integer(record.revision)?]));
    }
    if let WorkAction::BindWorkDelegation {
        parent_binding_receipt,
        ..
    } = &command.action
    {
        guard_observed_receipt(
            &parent_binding_receipt.mutation_id,
            &parent_binding_receipt.session_id,
            facts.parent_binding_receipt.as_ref(),
            statements,
        )?;
    }
    if let (WorkAction::BindWorkDelegation { binding, .. }, Some(parent_effect)) =
        (&command.action, &facts.parent_effect)
    {
        statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_effects WHERE session_id=?1 AND invocation_id=?2 AND lifecycle=?3 AND revision=?4 AND record_json=?5)",vec![text(&binding.initiator_session_id),text(&binding.invocation_id),payload::integer(parent_effect.recipient_lifecycle)?,payload::integer(parent_effect.revision)?,text(encode(parent_effect)?)]));
    }
    if let WorkAction::AcknowledgeTerminalObligation { receipt, .. } = &command.action {
        guard_observed_receipt(
            &receipt.mutation_id,
            &receipt.session_id,
            facts.parent_binding_receipt.as_ref(),
            statements,
        )?;
    }
    Ok(())
}

fn guard_observed_receipt(
    mutation_id: &str,
    session_id: &str,
    receipt: Option<&WorkReceipt>,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    if let Some(receipt) = receipt {
        statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_work_receipts WHERE mutation_id=?1 AND session_id=?2 AND resolution_json=?3)",vec![text(mutation_id),text(session_id),text(encode(&WorkResolution::Applied {receipt:receipt.clone()})?)]));
    } else {
        statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE EXISTS(SELECT 1 FROM session_work_receipts WHERE mutation_id=?1 AND session_id=?2 AND json_extract(resolution_json,'$.status')='applied')",vec![text(mutation_id),text(session_id)]));
    }
    Ok(())
}

fn append_write(
    command: &WorkCommand,
    write: &WorkWrite,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    match write {
        WorkWrite::Head {
            expected_change_seq,
            record,
        } => {
            statements.push(SqlStatement::checked("UPDATE session_work_head SET revision=?3,next_delivery_seq=?4,record_json=?5 WHERE session_id=?1 AND lifecycle=?2 AND revision=?6",vec![text(&command.session_id),payload::integer(record.lifecycle)?,payload::integer(record.change_seq)?,payload::integer(record.next_delivery_seq)?,text(encode(record)?),payload::integer(*expected_change_seq)?],1));
            statements.push(SqlStatement::new(
                GUARD_HEAD,
                vec![
                    text(&command.session_id),
                    payload::integer(record.lifecycle)?,
                    payload::integer(record.change_seq)?,
                ],
            ));
        }
        WorkWrite::Delivery {
            expected_revision,
            record,
        } => write_delivery(command, *expected_revision, record, statements)?,
        WorkWrite::Processing {
            expected_revision,
            record,
        } => write_processing(command, *expected_revision, record, statements)?,
        WorkWrite::Effect {
            expected_revision,
            record,
        } => write_effect(command, *expected_revision, record, statements)?,
        WorkWrite::Draft {
            expected_revision,
            record,
        } => write_draft(command, *expected_revision, record, statements)?,
        WorkWrite::Transcript { payload: message } => transcript(command, message, statements)?,
        WorkWrite::Control {
            expected_revision,
            record,
        } => {
            statements.push(SqlStatement::new(
                "INSERT OR IGNORE INTO session_control_state(session_id,state_json) VALUES (?1,?2)",
                vec![
                    text(&command.session_id),
                    text(encode(
                        &peri_acp_types::session_resources::ControlState::default(),
                    )?),
                ],
            ));
            statements.push(SqlStatement::checked("UPDATE session_control_state SET state_json=?2 WHERE session_id=?1 AND json_extract(state_json,'$.revision')=?3",vec![text(&command.session_id),text(encode(record)?),payload::integer(*expected_revision)?],1));
            statements.push(SqlStatement::new(
                GUARD_CONTROL,
                vec![
                    text(&command.session_id),
                    payload::integer(record.lifecycle)?,
                    payload::integer(record.revision)?,
                    payload::integer(record.control_generation)?,
                ],
            ));
        }
        WorkWrite::Admission {
            expected_entering_mutation_id,
            record,
        } => auxiliary(
            command,
            "admission",
            &record.admission.admission_id,
            record.admission.lifecycle,
            record,
            expected_entering_mutation_id.as_deref(),
            &record.entering_mutation_id,
            statements,
        )?,
        WorkWrite::RecoveryDescriptor {
            expected_revision,
            record,
        } => auxiliary(
            command,
            "recoveryDescriptor",
            &record.descriptor_id,
            record.recipient_lifecycle,
            record,
            expected_revision
                .map(|revision| revision.to_string())
                .as_deref(),
            &record.revision.to_string(),
            statements,
        )?,
        WorkWrite::TerminalObligation {
            expected_acknowledged,
            record,
        } => auxiliary(
            command,
            "terminal",
            &record.admission_id,
            command.recipient_lifecycle,
            record,
            Some(if *expected_acknowledged { "1" } else { "0" }),
            if record.acknowledgement.is_some() {
                "1"
            } else {
                "0"
            },
            statements,
        )?,
        WorkWrite::LegacyEvidence { record } => auxiliary(
            command,
            "legacy",
            &record.record_id,
            command.recipient_lifecycle,
            record,
            None,
            "0",
            statements,
        )?,
    }
    Ok(())
}

fn expected(value: Option<u64>) -> SessionResourceResult<SqlParam> {
    value
        .map(payload::integer)
        .transpose()
        .map(|value| value.unwrap_or(SqlParam::Null))
}

fn write_delivery(
    command: &WorkCommand,
    revision: Option<u64>,
    record: &Delivery,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    statements.push(payload::guard(&record.publication.event.content.content)?);
    let event = &record.publication.event;
    let key = encode(&(event.producer_namespace.as_str(), event.event_id.as_str()))?;
    statements.push(SqlStatement::new(
        "INSERT OR IGNORE INTO session_work_events(event_key,event_json) VALUES (?1,?2)",
        vec![text(&key), text(encode(event)?)],
    ));
    statements.push(SqlStatement::new("INSERT INTO session_work_events(event_key) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_work_events WHERE event_key=?1 AND event_json=?2)",vec![text(key),text(encode(event)?)]));
    let params = vec![
        text(&record.delivery_id),
        text(&command.session_id),
        payload::integer(record.recipient_lifecycle)?,
        payload::integer(record.revision)?,
        payload::integer(record.admission_sequence)?,
        text(&event.producer_namespace),
        text(&event.event_id),
        enum_text(&record.publication.purpose)?,
        enum_text(&record.obligation)?,
        optional(&record.processing_id),
        record
            .batch_ordinal
            .map(|ordinal| SqlParam::Integer(ordinal.into()))
            .unwrap_or(SqlParam::Null),
        text(encode(record)?),
        expected(revision)?,
    ];
    statements.push(SqlStatement::checked("INSERT INTO session_deliveries(delivery_id,session_id,lifecycle,revision,sequence,producer_namespace,event_id,purpose,status,processing_id,batch_ordinal,record_json) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12 WHERE ?13 IS NULL ON CONFLICT(delivery_id) DO NOTHING",params.clone(),if revision.is_none() {1} else {0}));
    statements.push(SqlStatement::checked("UPDATE session_deliveries SET revision=?4,status=?9,processing_id=?10,batch_ordinal=?11,record_json=?12 WHERE delivery_id=?1 AND session_id=?2 AND lifecycle=?3 AND revision=?13",params,if revision.is_some() {1} else {0}));
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_deliveries WHERE delivery_id=?1 AND session_id=?2 AND lifecycle=?3 AND revision=?4 AND record_json=?5)",vec![text(&record.delivery_id),text(&command.session_id),payload::integer(record.recipient_lifecycle)?,payload::integer(record.revision)?,text(encode(record)?)]));
    Ok(())
}

fn write_processing(
    command: &WorkCommand,
    revision: Option<u64>,
    record: &Processing,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    if let Some(reference) = &record.request {
        statements.push(payload::guard(reference)?);
    }
    if let Some(response) = &record.response {
        statements.push(payload::guard(&response.content)?);
    }
    let params = vec![
        text(&record.processing_id),
        text(&command.session_id),
        payload::integer(record.recipient_lifecycle)?,
        payload::integer(record.revision)?,
        payload::integer(record.phase_sequence)?,
        enum_text(&record.stage)?,
        record
            .delegation
            .as_ref()
            .map(|delegation| text(&delegation.parent_session_id))
            .unwrap_or(SqlParam::Null),
        record
            .delegation
            .as_ref()
            .map(|delegation| text(&delegation.delegation_id))
            .unwrap_or(SqlParam::Null),
        text(encode(record)?),
        expected(revision)?,
    ];
    statements.push(SqlStatement::checked("INSERT INTO session_processing(processing_id,session_id,lifecycle,revision,phase_sequence,phase,parent_session,delegation_id,record_json) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9 WHERE ?10 IS NULL ON CONFLICT(processing_id) DO NOTHING",params.clone(),if revision.is_none() {1} else {0}));
    statements.push(SqlStatement::checked("UPDATE session_processing SET revision=?4,phase_sequence=?5,phase=?6,parent_session=?7,delegation_id=?8,record_json=?9 WHERE processing_id=?1 AND session_id=?2 AND lifecycle=?3 AND revision=?10",params,if revision.is_some() {1} else {0}));
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_processing WHERE processing_id=?1 AND session_id=?2 AND lifecycle=?3 AND revision=?4 AND record_json=?5)",vec![text(&record.processing_id),text(&command.session_id),payload::integer(record.recipient_lifecycle)?,payload::integer(record.revision)?,text(encode(record)?)]));
    Ok(())
}

fn write_effect(
    command: &WorkCommand,
    revision: Option<u64>,
    record: &Effect,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    statements.push(payload::guard(&record.intent.arguments)?);
    statements.push(payload::guard(&record.intent.effective_arguments)?);
    if let Some(
        InvocationOutcome::Completed { result }
        | InvocationOutcome::Failed { result }
        | InvocationOutcome::Cancelled { result, .. },
    ) = &record.outcome
    {
        statements.push(payload::guard(&result.content)?);
    }
    let params = vec![
        text(&record.invocation_id),
        text(&command.session_id),
        payload::integer(record.recipient_lifecycle)?,
        payload::integer(record.revision)?,
        optional(&record.processing_id),
        payload::integer(record.phase_sequence)?,
        text(&record.intent.tool_call_id),
        enum_text(&record.status)?,
        record
            .binding
            .as_ref()
            .map(|binding| text(&binding.owner_identity))
            .unwrap_or(SqlParam::Null),
        record
            .binding
            .as_ref()
            .map(|binding| text(&binding.owner_task_id))
            .unwrap_or(SqlParam::Null),
        SqlParam::Null,
        record
            .delegation
            .as_ref()
            .map(|delegation| text(&delegation.parent_session_id))
            .unwrap_or(SqlParam::Null),
        record
            .delegation
            .as_ref()
            .map(|delegation| text(&delegation.delegation_id))
            .unwrap_or(SqlParam::Null),
        text(encode(record)?),
        expected(revision)?,
    ];
    statements.push(SqlStatement::checked("INSERT INTO session_effects(invocation_id,session_id,lifecycle,revision,processing_id,phase_sequence,tool_call_id,status,owner_identity,owner_task_id,child_session,parent_session,delegation_id,record_json) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14 WHERE ?15 IS NULL ON CONFLICT(invocation_id) DO NOTHING",params.clone(),if revision.is_none() {1} else {0}));
    statements.push(SqlStatement::checked("UPDATE session_effects SET revision=?4,processing_id=?5,phase_sequence=?6,status=?8,owner_identity=?9,owner_task_id=?10,child_session=?11,parent_session=?12,delegation_id=?13,record_json=?14 WHERE invocation_id=?1 AND session_id=?2 AND lifecycle=?3 AND revision=?15",params,if revision.is_some() {1} else {0}));
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_effects WHERE invocation_id=?1 AND session_id=?2 AND lifecycle=?3 AND revision=?4 AND record_json=?5)",vec![text(&record.invocation_id),text(&command.session_id),payload::integer(record.recipient_lifecycle)?,payload::integer(record.revision)?,text(encode(record)?)]));
    Ok(())
}

fn write_draft(
    command: &WorkCommand,
    revision: Option<u64>,
    record: &StagedUserInput,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    statements.push(payload::guard(&record.content)?);
    let params = vec![
        text(&command.session_id),
        payload::integer(record.recipient_lifecycle)?,
        text(&record.input_id),
        payload::integer(record.revision)?,
        payload::integer(record.sequence)?,
        payload::integer(record.publication_generation)?,
        enum_text(&record.status)?,
        text(encode(record)?),
        expected(revision)?,
    ];
    statements.push(SqlStatement::checked("INSERT INTO session_inputs(session_id,lifecycle,input_id,revision,fifo_seq,generation,status,record_json) SELECT ?1,?2,?3,?4,?5,?6,?7,?8 WHERE ?9 IS NULL ON CONFLICT(session_id,lifecycle,input_id) DO NOTHING",params.clone(),if revision.is_none() {1} else {0}));
    statements.push(SqlStatement::checked("UPDATE session_inputs SET revision=?4,fifo_seq=?5,generation=?6,status=?7,record_json=?8 WHERE session_id=?1 AND lifecycle=?2 AND input_id=?3 AND revision=?9",params,if revision.is_some() {1} else {0}));
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_inputs WHERE session_id=?1 AND lifecycle=?2 AND input_id=?3 AND revision=?4 AND record_json=?5)",vec![text(&command.session_id),payload::integer(record.recipient_lifecycle)?,text(&record.input_id),payload::integer(record.revision)?,text(encode(record)?)]));
    Ok(())
}

pub(super) fn auxiliary(
    command: &WorkCommand,
    kind: &str,
    subject: &str,
    lifecycle: u64,
    record: &impl Serialize,
    previous: Option<&str>,
    token: &str,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    let (reference, payload_plan) =
        payload::prepare_bytes(&command.session_id, kind, encode(record)?.as_bytes())?;
    statements.extend(payload_plan);
    let identity = format!(
        "record:{}",
        encode(&(command.session_id.as_str(), lifecycle, kind, subject))?
    );
    let params = vec![
        text(identity),
        text(&command.session_id),
        text(&reference.sha256),
        text(encode(&reference)?),
        text(&reference.storage_scope),
        text(&reference.payload_id),
        text(kind),
        text(subject),
        payload::integer(lifecycle)?,
        text(token),
        previous.map(text).unwrap_or(SqlParam::Null),
    ];
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id,session_id,digest,command_json,command_scope,command_payload_id,kind,subject_id,lifecycle,guard_token,record_json,reconciled) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?4,1 WHERE ?11 IS NULL OR NOT EXISTS(SELECT 1 FROM session_work_commands WHERE mutation_id=?1) ON CONFLICT(mutation_id) DO NOTHING",params.clone()));
    statements.push(SqlStatement::new("UPDATE session_work_commands SET digest=?3,command_json=?4,command_scope=?5,command_payload_id=?6,guard_token=?10,record_json=?4 WHERE mutation_id=?1 AND session_id=?2 AND lifecycle=?9 AND kind=?7 AND guard_token=?11",params));
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_work_commands WHERE session_id=?1 AND lifecycle=?2 AND kind=?3 AND subject_id=?4 AND guard_token=?5 AND record_json=?6)",vec![text(&command.session_id),payload::integer(lifecycle)?,text(kind),text(subject),text(token),text(encode(&reference)?)]));
    Ok(())
}

fn transcript(
    command: &WorkCommand,
    message: &WorkPayload,
    statements: &mut Vec<SqlStatement>,
) -> SessionResourceResult<()> {
    message.validate()?;
    statements.push(payload::guard(&message.content)?);
    let identity = message.message_id.as_uuid().to_string();
    let params = vec![
        text(&identity),
        text(&command.session_id),
        text(&message.role),
        text(encode(&message.content)?),
    ];
    statements.push(SqlStatement::new("UPDATE threads SET message_count=message_count+1,updated_at=?3 WHERE id=?2 AND NOT EXISTS(SELECT 1 FROM messages WHERE message_id=?1)",vec![text(&identity),text(&command.session_id),text(peri_time::now_utc_rfc3339())]));
    statements.push(SqlStatement::new("INSERT OR IGNORE INTO messages(message_id,thread_id,role,content_ref,transcript_seq) VALUES (?1,?2,?3,?4,COALESCE((SELECT MAX(transcript_seq)+1 FROM messages WHERE thread_id=?2),1))",params.clone()));
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM messages WHERE message_id=?1 AND thread_id=?2 AND role=?3 AND content_ref=?4)",params));
    Ok(())
}
