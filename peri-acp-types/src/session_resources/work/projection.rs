use super::*;

impl InvocationRecord {
    pub fn settled_projection(
        &self,
        session_id: &str,
    ) -> SessionResourceResult<Option<WorkPayload>> {
        if self.status != InvocationStatus::Settled {
            return Ok(None);
        }
        match &self.outcome {
            Some(
                InvocationOutcome::Completed { result } | InvocationOutcome::Failed { result },
            ) => Ok(Some(result.clone())),
            Some(InvocationOutcome::Cancelled { evidence }) => {
                if session_id.is_empty() || evidence.is_empty() {
                    return Err(invalid("cancelled invocation projection lacks evidence"));
                }
                let key = serde_json::to_vec(&(
                    session_id,
                    self.recipient_lifecycle,
                    self.intent.invocation_id.as_str(),
                    "cancelled",
                ))
                .map_err(|_| invalid("invalid invocation projection identity"))?;
                let hash = Sha256::digest(key);
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(&hash[..16]);
                bytes[6] = (bytes[6] & 0x0f) | 0x50;
                bytes[8] = (bytes[8] & 0x3f) | 0x80;
                let payload = WorkPayload::from_payload(&PersistedPayload::Message(
                    crate::messages::BaseMessage::tool_result(
                        &self.intent.tool_call_id,
                        format!("Invocation cancelled: {evidence}"),
                    ),
                ))?;
                Ok(Some(payload.with_message_id(MessageId::from(
                    uuid::Uuid::from_bytes(bytes),
                ))?))
            }
            None => Err(invalid("settled invocation has no outcome")),
        }
    }
}
