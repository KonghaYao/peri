//! 当前会话运行期的消费证据。只有实际请求的成功响应跨过持久化 barrier 后才记账。

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;

use sha2::{Digest, Sha256};

use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, MessageId};
use crate::session::MessageTranscript;

const MAX_EXPOSED_MESSAGES: usize = 32_768;

#[derive(Debug, Default)]
pub struct RequestExposure {
    members: Vec<(MessageId, [u8; 32])>,
    synchronous_terminal_calls: Vec<String>,
}

impl RequestExposure {
    pub(crate) fn capture(
        messages: &[BaseMessage],
        transcript: &MessageTranscript,
    ) -> AgentResult<Self> {
        let canonical: Vec<_> = messages
            .iter()
            .rev()
            .take(MAX_EXPOSED_MESSAGES)
            .filter(|message| {
                transcript
                    .get(message.id())
                    .and_then(|entry| entry.as_message())
                    .is_some_and(|source| {
                        source.message_content() == message.message_content()
                            && source.tool_calls() == message.tool_calls()
                    })
            })
            .collect();
        let members = canonical
            .iter()
            .map(|message| Ok((message.id(), fingerprint(message)?)))
            .collect::<AgentResult<_>>()?;
        // Cold history becomes eligible only after a successful request actually exposes
        // its exact canonical result. Persisted typed synchronous terminal evidence is
        // sufficient; task-bearing results still require their live owner settlement.
        let synchronous_terminal_calls = canonical
            .iter()
            .rev()
            .filter_map(|message| {
                match transcript
                    .get(message.id())
                    .and_then(|entry| entry.as_message())
                {
                    Some(BaseMessage::Tool {
                        tool_call_id,
                        execution: Some(evidence),
                        ..
                    }) if evidence.task_id.is_none() && is_terminal(evidence.status) => {
                        Some(tool_call_id.clone())
                    }
                    _ => None,
                }
            })
            .collect();
        Ok(Self {
            members,
            synchronous_terminal_calls,
        })
    }
}

/// 有界、非持久化；遗忘证据只会增加保护，绝不推断冷历史已被消费。
#[derive(Debug, Default)]
pub struct ExposureLedger {
    messages: HashMap<MessageId, [u8; 32]>,
    order: VecDeque<MessageId>,
    closed_calls: HashSet<String>,
    closed_order: VecDeque<String>,
}

impl ExposureLedger {
    pub(crate) fn confirm(&mut self, receipt: &RequestExposure) {
        self.confirm_closed(receipt.synchronous_terminal_calls.iter().cloned());
        for &(id, digest) in receipt.members.iter().rev() {
            if self.messages.insert(id, digest).is_none() {
                self.order.push_back(id);
            }
        }
        while self.order.len() > MAX_EXPOSED_MESSAGES {
            if let Some(id) = self.order.pop_front() {
                self.messages.remove(&id);
            }
        }
    }

    pub(crate) fn confirm_closed(&mut self, calls: impl IntoIterator<Item = String>) {
        for id in calls {
            if self.closed_calls.insert(id.clone()) {
                self.closed_order.push_back(id);
            }
        }
        while self.closed_order.len() > MAX_EXPOSED_MESSAGES {
            if let Some(id) = self.closed_order.pop_front() {
                self.closed_calls.remove(&id);
            }
        }
    }

    pub(crate) fn exposed_ids(
        &self,
        transcript: &MessageTranscript,
    ) -> AgentResult<HashSet<MessageId>> {
        let mut exposed = HashSet::new();
        for entry in transcript.entries() {
            let Some(expected) = self.messages.get(&entry.id()) else {
                continue;
            };
            if let Some(message) = entry.as_message() {
                if fingerprint(message)? == *expected {
                    exposed.insert(entry.id());
                }
            }
        }
        Ok(exposed)
    }

    pub(crate) fn closed_calls(&self) -> HashSet<String> {
        self.closed_calls.clone()
    }
}

fn is_terminal(status: crate::tools::ToolExecutionStatus) -> bool {
    matches!(
        status,
        crate::tools::ToolExecutionStatus::Completed
            | crate::tools::ToolExecutionStatus::Failed
            | crate::tools::ToolExecutionStatus::Cancelled
    )
}

pub(crate) fn fingerprint(message: &BaseMessage) -> AgentResult<[u8; 32]> {
    struct DigestWriter(Sha256);
    impl Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, message)
        .map_err(|error| AgentError::Other(anyhow::Error::new(error)))?;
    Ok(writer.0.finalize().into())
}

#[cfg(test)]
#[path = "exposure_test.rs"]
mod tests;
