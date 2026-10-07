use super::*;
use crate::messages::BaseMessage;
use crate::store::PersistedPayload;

pub fn validate_reason_response_intents(
    response: &WorkPayload,
    payload: &PersistedPayload,
    intents: &[InvocationIntent],
) -> Result<(), WorkRejection> {
    if response.validate().is_err()
        || response.role != "assistant"
        || response.message_id != payload.id()
    {
        return Err(WorkRejection::Conflict);
    }
    let message = payload.as_message().ok_or(WorkRejection::Conflict)?;
    validate_response_intents(message, intents)
}

pub(super) fn validate_response_intents(
    response: &BaseMessage,
    intents: &[InvocationIntent],
) -> Result<(), WorkRejection> {
    if !matches!(response, BaseMessage::Ai { .. })
        || intents.len() > MAX_WORK_PAGE_SIZE as usize
        || response.tool_calls().len() != intents.len()
    {
        return Err(WorkRejection::Conflict);
    }
    let mut call_ids = Vec::new();
    let mut invocation_ids = Vec::new();
    for call in response.tool_calls() {
        if call.id.is_empty() || call.name.is_empty() || call_ids.contains(&call.id) {
            return Err(WorkRejection::Conflict);
        }
        call_ids.push(call.id.clone());
        let mut matching = intents
            .iter()
            .filter(|intent| intent.tool_call_id == call.id);
        let intent = matching.next().ok_or(WorkRejection::Conflict)?;
        if matching.next().is_some()
            || invocation_ids.contains(&intent.invocation_id)
            || super::effect::validate_intent(intent).is_err()
            || intent.tool_name != call.name
        {
            return Err(WorkRejection::Conflict);
        }
        invocation_ids.push(intent.invocation_id.clone());
        let arguments = serde_json::to_vec(&call.arguments).map_err(|_| WorkRejection::Conflict)?;
        if format!("{:x}", Sha256::digest(&arguments)) != intent.arguments_digest
            || arguments.len() as u64 != intent.arguments.byte_length
        {
            return Err(WorkRejection::Conflict);
        }
    }
    Ok(())
}
