use super::*;
use peri_acp_types::session_resources::work::{WorkAction, WorkDecision};

const INSERT_EVENT: &str =
    "INSERT OR IGNORE INTO session_work_events(event_key,event_json) VALUES (?1,?2)";
const GUARD_EVENT: &str = "INSERT INTO session_work_state(session_id,state_json) SELECT NULL,NULL WHERE NOT EXISTS (SELECT 1 FROM session_work_events WHERE event_key=?1 AND event_json=?2)";
const INSERT_PROJECTION: &str =
    "INSERT OR IGNORE INTO messages(message_id,thread_id,role,content) VALUES (?1,?2,?3,?4)";
const GUARD_PROJECTION: &str = "INSERT INTO session_work_state(session_id,state_json) SELECT NULL,NULL WHERE NOT EXISTS (SELECT 1 FROM messages WHERE message_id=?1 AND thread_id=?2 AND role=?3 AND content=?4)";
const REFRESH_COUNTS: &str = "UPDATE threads SET updated_at=?1,message_count=(SELECT COUNT(*) FROM messages WHERE thread_id=?2) WHERE id=?2";

pub(in crate::sessions) fn mutation_effects(
    command: &WorkCommand,
    current: &WorkState,
    current_json: Option<String>,
    control: &ControlState,
    reduction: &WorkReduction,
) -> SessionResourceResult<Vec<WorkEffect>> {
    let initial = match current_json {
        Some(json) => json,
        None => encode(current)?,
    };
    let mut effects = command_effects(command)?;
    effects.extend([
        WorkEffect::texts(INSERT_STATE, [command.session_id.clone(), initial.clone()]),
        WorkEffect::texts(
            GUARD_STATE,
            [
                command.session_id.clone(),
                initial,
                encode(&ControlState::default())?,
                encode(control)?,
            ],
        ),
    ]);
    if reduction.receipt.decision == WorkDecision::Accepted {
        if let WorkAction::BindWorkDelegation {
            binding,
            parent_binding_receipt,
            ..
        } = &command.action
        {
            effects.push(WorkEffect::texts("INSERT INTO session_work_state(session_id,state_json) SELECT NULL,NULL WHERE NOT EXISTS(SELECT 1 FROM session_work_commands c JOIN session_work_receipts r ON r.mutation_id=c.mutation_id AND r.session_id=c.session_id AND r.digest=c.digest JOIN session_work_state s ON s.session_id=c.session_id WHERE c.mutation_id=?1 AND c.session_id=?2 AND r.resolution_json=?3 AND json_extract(c.command_json,'$.action.kind')='reconcileTaskBinding' AND json_extract(c.command_json,'$.action.binding')=?4 AND EXISTS(SELECT 1 FROM json_each(s.state_json,'$.taskBindings') b WHERE b.key=?5 AND b.value=?4))", [parent_binding_receipt.mutation_id.clone(),binding.initiator_session_id.clone(),encode(&WorkResolution::Applied { receipt: parent_binding_receipt.clone() })?,encode(binding)?,binding.invocation_id.clone()]));
        }
        if let WorkAction::AcknowledgeTerminalObligation {
            admission_id,
            receipt,
            ..
        } = &command.action
        {
            let parent_command = current
                .terminal_obligations
                .get(admission_id)
                .ok_or_else(|| corrupt("missing terminal obligation"))?;
            effects.push(WorkEffect::texts("INSERT INTO session_work_state(session_id,state_json) SELECT NULL,NULL WHERE NOT EXISTS(SELECT 1 FROM session_work_receipts WHERE mutation_id=?1 AND session_id=?2 AND digest=?3 AND resolution_json=?4)", [parent_command.mutation_id.clone(),parent_command.session_id.clone(),parent_command.digest()?,encode(&WorkResolution::Applied { receipt: receipt.clone() })?]));
        }
        if let Some(next_control) = &reduction.control {
            effects.push(WorkEffect::texts("INSERT INTO session_control_state(session_id,state_json) VALUES (?1,?2) ON CONFLICT(session_id) DO UPDATE SET state_json=excluded.state_json", [command.session_id.clone(), encode(next_control)?]));
        }
        for event in &reduction.events {
            let key = encode(&(event.producer_namespace.as_str(), event.event_id.as_str()))?;
            let json = encode(event)?;
            effects.push(WorkEffect::texts(INSERT_EVENT, [key.clone(), json.clone()]));
            effects.push(WorkEffect::texts(GUARD_EVENT, [key, json]));
        }
        for projection in &reduction.projections {
            let params = [
                projection.message_id.as_uuid().to_string(),
                command.session_id.clone(),
                projection.role.clone(),
                projection.serialized.clone(),
            ];
            effects.push(WorkEffect::texts(INSERT_PROJECTION, params.clone()));
            effects.push(WorkEffect::texts(GUARD_PROJECTION, params));
        }
        if !reduction.projections.is_empty() {
            effects.push(WorkEffect::texts(
                REFRESH_COUNTS,
                [peri_time::now_utc_rfc3339(), command.session_id.clone()],
            ));
            let payloads = reduction
                .projections
                .iter()
                .map(|payload| {
                    peri_acp_types::store::deserialize_persisted_payload(&payload.serialized)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| corrupt("work projection is not readable"))?;
            let messages: Vec<_> = payloads
                .iter()
                .filter_map(peri_acp_types::store::PersistedPayload::as_message)
                .cloned()
                .collect();
            if let Some(title) = crate::sessions::canonical::extract_title(&messages) {
                effects.push(WorkEffect::texts(
                    "UPDATE threads SET title=?1 WHERE id=?2 AND title IS NULL",
                    [title, command.session_id.clone()],
                ));
            }
        }
        effects.push(WorkEffect::texts(
            UPDATE_STATE,
            [command.session_id.clone(), encode(&reduction.state)?],
        ));
    }
    let resolution = WorkResolution::Applied {
        receipt: reduction.receipt.clone(),
    };
    effects.push(WorkEffect::texts(
        INSERT_RECEIPT,
        [
            command.mutation_id.clone(),
            command.session_id.clone(),
            command.digest()?,
            encode(&resolution)?,
        ],
    ));
    if matches!(command.action, WorkAction::RegisterAdmission { .. })
        && reduction.control.is_some()
        && reduction.receipt.decision != WorkDecision::Accepted
    {
        return Err(corrupt("rejected admission produced control effects"));
    }
    Ok(effects)
}

#[cfg(all(test, not(target_os = "emscripten")))]
#[path = "effects_test.rs"]
mod tests;
