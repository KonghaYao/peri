use super::*;
use peri_acp_types::session_resources::work::{WorkAvailability, WorkAvailabilityState};

// 只投影事实；生命周期、义务、batch 上限和 Required 补位均由领域层判定。
// json_each 仍需解析 SQLite 中的 blob，但不将正文或历史请求送入 Rust / 远端结果集。
pub(in crate::sessions) const READ_AVAILABILITY: &str = r#"
SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1), control.state_json,
CASE WHEN state.session_id IS NULL THEN NULL ELSE json_object(
    'revision', json(state.state_json -> '$.revision'),
    'maxBatchSize', json(state.state_json -> '$.limits.maxBatchSize'),
    'deliveries', json((SELECT json_group_object(entry.key,json_object(
        'recipientLifecycle', json(entry.value -> '$.recipientLifecycle'),
        'admissionSequence', json(entry.value -> '$.admissionSequence'),
        'requirement', json_extract(entry.value,'$.publication.policy.requirement'),
        'batchId', json_extract(entry.value,'$.batchId'),
        'disposition', json_extract(entry.value,'$.disposition')
    )) FROM json_each(state.state_json,'$.deliveries') AS entry)),
    'obligations', json((SELECT json_group_object(entry.key,json_extract(entry.value,'$.status'))
        FROM json_each(state.state_json,'$.obligations') AS entry)),
    'works', json((SELECT json_group_object(entry.key,json_object(
        'workId', json_extract(entry.value,'$.workId'),
        'batchId', json_extract(entry.value,'$.batchId'),
        'stage', json_extract(entry.value,'$.stage')
    )) FROM json_each(state.state_json,'$.works') AS entry)),
    'batches', json((SELECT json_group_object(entry.key,json_object(
        'batchId', json_extract(entry.value,'$.batchId'),
        'recipientLifecycle', json(entry.value -> '$.recipientLifecycle'),
        'processingDeliveryIds', json_extract(entry.value,'$.processingDeliveryIds')
    )) FROM json_each(state.state_json,'$.batches') AS entry)),
    'admissions', json((SELECT json_group_object(entry.key,json_object(
        'sessionId', json_extract(entry.value,'$.admission.sessionId'),
        'workId', json_extract(entry.value,'$.admission.workId'),
        'lifecycle', json(entry.value -> '$.admission.lifecycle'),
        'execution', json_extract(entry.value,'$.admission.execution')
    )) FROM json_each(state.state_json,'$.admissions') AS entry)),
    'legacyUnknown', json((SELECT json_group_array(entry.key)
        FROM json_each(state.state_json,'$.legacyUnknown') AS entry)),
    'terminalObligations', json((SELECT json_group_array(entry.key)
        FROM json_each(state.state_json,'$.terminalObligations') AS entry)),
    'terminalAcknowledgements', json((SELECT json_group_array(entry.key)
        FROM json_each(state.state_json,'$.terminalAcknowledgements') AS entry))
) END, EXISTS(SELECT 1 FROM messages WHERE thread_id=?1)
FROM (SELECT 1)
LEFT JOIN session_control_state AS control ON control.session_id=?1
LEFT JOIN session_work_state AS state ON state.session_id=?1
"#;

pub(in crate::sessions) fn availability(
    exists: bool,
    control: Option<&str>,
    facts: Option<&str>,
    has_history: bool,
) -> SessionResourceResult<WorkAvailability> {
    if !exists && control.is_none() && facts.is_none() {
        return Err(SessionResourceError::new(
            peri_acp_types::session_resources::SessionResourceErrorKind::NotFound,
        ));
    }
    let state = match facts {
        Some(json) => decode(json)?,
        None => WorkAvailabilityState::from(&state(None, has_history)?),
    };
    Ok(WorkAvailability {
        control: crate::sessions::control::state(control)?,
        state,
    })
}
