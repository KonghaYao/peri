use super::{SqlParam, SqlStatement};
use crate::sessions::{failure::corrupt, work::encode};
use peri_acp_types::session_resources::control::decide_control;
use peri_acp_types::session_resources::{
    ControlAction, ControlDecision, ControlResolution, ControlState, ControlStatus,
    SessionResourceResult,
};

pub(crate) fn creation_guard(session_id: &str) -> Vec<SqlStatement> {
    vec![SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE EXISTS(SELECT 1 FROM session_control_state WHERE session_id=?1 UNION ALL SELECT 1 FROM session_work_head WHERE session_id=?1 UNION ALL SELECT 1 FROM session_work_commands WHERE session_id=?1)",vec![SqlParam::Text(session_id.into())])]
}

pub(crate) fn history_guard(session_id: &str) -> Vec<SqlStatement> {
    vec![SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE EXISTS(WITH RECURSIVE scope(id) AS (SELECT ?1 UNION ALL SELECT threads.id FROM threads JOIN scope ON threads.parent_thread_id=scope.id) SELECT 1 FROM session_work_commands WHERE session_id IN (SELECT id FROM scope) AND kind='mutation' AND reconciled=0) OR EXISTS(SELECT 1 FROM session_work_head WHERE session_id=?1 AND (json_extract(record_json,'$.requiredCount')<>0 OR json_extract(record_json,'$.unresolvedEffects')<>0 OR json_extract(record_json,'$.terminalObligations')<>0 OR json_extract(record_json,'$.legacyUnknown')<>0 OR json_extract(record_json,'$.currentProcessingId') IS NOT NULL))",vec![SqlParam::Text(session_id.into())])]
}

pub(crate) fn tombstone_plan(
    session_id: &str,
    current: &ControlState,
) -> SessionResourceResult<Vec<SqlStatement>> {
    let mut statements = history_guard(session_id);
    statements.push(SqlStatement::new("INSERT INTO session_control_state(session_id,state_json) VALUES (?1,?2) ON CONFLICT(session_id) DO NOTHING",vec![SqlParam::Text(session_id.into()),SqlParam::Text(encode(current)?)]));
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_control_state WHERE session_id=?1 AND json_extract(state_json,'$.lifecycle')=?2 AND json_extract(state_json,'$.revision')=?3 AND json_extract(state_json,'$.controlGeneration')=?4)",vec![SqlParam::Text(session_id.into()),super::payload::integer(current.lifecycle)?,super::payload::integer(current.revision)?,super::payload::integer(current.control_generation)?]));
    let mut state = current.clone();
    if state.status == ControlStatus::Closed {
        return Ok(statements);
    }
    let actions = if state.status == ControlStatus::Closing {
        vec![ControlAction::FinishClose]
    } else {
        vec![ControlAction::Close, ControlAction::FinishClose]
    };
    for action in actions {
        let command = crate::sessions::control::close_command(session_id, &state, action)?;
        let receipt = decide_control(&command, &state);
        if receipt.decision != ControlDecision::Accepted {
            return Err(corrupt(
                "session deletion cannot establish a closed control tombstone",
            ));
        }
        statements.push(SqlStatement::new("INSERT INTO session_control_receipts(command_id,session_id,digest,resolution_json) VALUES (?1,?2,?3,?4)",vec![SqlParam::Text(command.command_id.clone()),SqlParam::Text(session_id.into()),SqlParam::Text(command.digest()?),SqlParam::Text(encode(&ControlResolution::Applied {receipt:receipt.clone()})?)]));
        state = receipt.state;
    }
    statements.push(SqlStatement::checked("UPDATE session_control_state SET state_json=?2 WHERE session_id=?1 AND json_extract(state_json,'$.revision')=?3",vec![SqlParam::Text(session_id.into()),SqlParam::Text(encode(&state)?),super::payload::integer(current.revision)?],1));
    statements.push(SqlStatement::new("INSERT INTO session_work_commands(mutation_id) SELECT NULL WHERE NOT EXISTS(SELECT 1 FROM session_control_state WHERE session_id=?1 AND json_extract(state_json,'$.revision')=?2 AND json_extract(state_json,'$.status')='closed')",vec![SqlParam::Text(session_id.into()),super::payload::integer(state.revision)?]));
    Ok(statements)
}
