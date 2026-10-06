use peri_acp_types::session_resources::work::{
    validate_reason_response_intents, WorkAction, WorkCommand, WorkDecision, WorkReceipt,
    WorkTransition,
};
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::store::deserialize_persisted_payload;

use super::{payload, SqlRows, SqlStatement};
use crate::sessions::failure::corrupt;

pub(crate) fn response_validation_plan(
    command: &WorkCommand,
    transition: &WorkTransition,
) -> SessionResourceResult<Option<SqlStatement>> {
    if transition.receipt.decision != WorkDecision::Accepted {
        return Ok(None);
    }
    match &command.action {
        WorkAction::CommitReasonResponseAndDispatchIntent { response, .. } => {
            payload::read_statement(&response.content).map(Some)
        }
        _ => Ok(None),
    }
}

pub(crate) fn validate_response_transition(
    command: &WorkCommand,
    transition: &mut WorkTransition,
    rows: &SqlRows,
) -> SessionResourceResult<()> {
    let WorkAction::CommitReasonResponseAndDispatchIntent {
        response,
        dispatch_intents,
        ..
    } = &command.action
    else {
        return Err(corrupt(
            "response validation requires a reason response command",
        ));
    };
    let evidence = payload::decode_evidence(&response.content, rows)?;
    let content = std::str::from_utf8(&evidence.bytes)
        .map_err(|_| corrupt("reason response payload is not UTF-8"))?;
    let decoded = deserialize_persisted_payload(content)
        .map_err(|_| corrupt("reason response payload is not readable"))?;
    let validation = validate_reason_response_intents(response, &decoded, dispatch_intents);
    if let Err(reason) = validation {
        tracing::warn!(session_id = %command.session_id, mutation_id = %command.mutation_id, ?reason, "reason response dispatch intent validation rejected");
        transition.receipt = WorkReceipt {
            session_id: command.session_id.clone(),
            mutation_id: command.mutation_id.clone(),
            before_revision: transition.receipt.before_revision,
            revision: transition.receipt.before_revision,
            decision: WorkDecision::Rejected { reason },
            delivery_id: None,
            admission_sequence: None,
            batch_id: None,
            work_id: None,
            work_revision: None,
            stage: None,
        };
        transition.writes.clear();
    }
    Ok(())
}
