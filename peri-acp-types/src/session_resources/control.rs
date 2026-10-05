use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{SessionResourceError, SessionResourceErrorKind, SessionResourceResult};
use crate::{identity::AttemptId, session::TurnId, thread::ThreadId};

#[cfg(test)]
#[path = "control_test.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlAttempt {
    pub turn_id: TurnId,
    pub attempt_id: AttemptId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ControlAction {
    Stop {
        target: ControlAttempt,
    },
    Pause,
    Resume,
    Close,
    Reopen,
    #[serde(skip_deserializing)]
    FinishClose,
    #[serde(skip_deserializing)]
    ObserveAttempt {
        target: Option<ControlAttempt>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlCommand {
    pub session_id: ThreadId,
    pub command_id: String,
    pub expected_lifecycle: u64,
    pub expected_revision: u64,
    pub expected_control_generation: u64,
    pub action: ControlAction,
}

impl ControlCommand {
    pub fn digest(&self) -> SessionResourceResult<String> {
        if self.session_id.is_empty() || self.command_id.is_empty() || self.command_id.len() > 256 {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::InvalidInput {
                    detail: "invalid control command identity".into(),
                },
            ));
        }
        let bytes = serde_json::to_vec(self).map_err(|_| {
            SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
                detail: "invalid control command".into(),
            })
        })?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlStatus {
    Active,
    Paused,
    Closing,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlState {
    pub lifecycle: u64,
    pub revision: u64,
    pub control_generation: u64,
    pub status: ControlStatus,
    pub attempt: Option<ControlAttempt>,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            lifecycle: 1,
            revision: 0,
            control_generation: 0,
            status: ControlStatus::Active,
            attempt: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlRejection {
    StaleLifecycle,
    StaleRevision,
    StaleControlGeneration,
    StaleAttempt,
    InvalidTransition,
    VersionExhausted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ControlDecision {
    Accepted,
    Rejected { reason: ControlRejection },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlReceipt {
    pub session_id: ThreadId,
    pub command_id: String,
    pub decision: ControlDecision,
    pub state: ControlState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ControlResolution {
    Applied { receipt: ControlReceipt },
    NotApplied,
    Unknown,
}

pub fn decide_control(command: &ControlCommand, current: &ControlState) -> ControlReceipt {
    let reject = |reason| ControlReceipt {
        session_id: command.session_id.clone(),
        command_id: command.command_id.clone(),
        decision: ControlDecision::Rejected { reason },
        state: current.clone(),
    };
    if command.expected_lifecycle != current.lifecycle {
        return reject(ControlRejection::StaleLifecycle);
    }
    if command.expected_revision != current.revision {
        return reject(ControlRejection::StaleRevision);
    }
    if command.expected_control_generation != current.control_generation {
        return reject(ControlRejection::StaleControlGeneration);
    }
    let mut next = current.clone();
    match &command.action {
        ControlAction::Stop { target } => {
            if current.attempt.as_ref() != Some(target) {
                return reject(ControlRejection::StaleAttempt);
            }
            if !matches!(
                current.status,
                ControlStatus::Active | ControlStatus::Paused
            ) {
                return reject(ControlRejection::InvalidTransition);
            }
            next.status = ControlStatus::Paused;
        }
        ControlAction::Pause | ControlAction::Resume => {
            if !matches!(
                current.status,
                ControlStatus::Active | ControlStatus::Paused
            ) {
                return reject(ControlRejection::InvalidTransition);
            }
            next.status = if command.action == ControlAction::Pause {
                ControlStatus::Paused
            } else {
                ControlStatus::Active
            };
        }
        ControlAction::Close => {
            if current.status == ControlStatus::Closed {
                return reject(ControlRejection::InvalidTransition);
            }
            next.status = ControlStatus::Closing;
        }
        ControlAction::Reopen => {
            if current.status != ControlStatus::Closed {
                return reject(ControlRejection::InvalidTransition);
            }
            let Some(lifecycle) = current.lifecycle.checked_add(1) else {
                return reject(ControlRejection::VersionExhausted);
            };
            next.lifecycle = lifecycle;
            next.status = ControlStatus::Active;
            next.attempt = None;
        }
        ControlAction::FinishClose => {
            if current.status != ControlStatus::Closing {
                return reject(ControlRejection::InvalidTransition);
            }
            next.status = ControlStatus::Closed;
            next.attempt = None;
        }
        ControlAction::ObserveAttempt { target } => {
            if current.attempt.is_some() && target.is_some() && current.attempt != *target {
                return reject(ControlRejection::StaleAttempt);
            }
            if target.is_some() && current.status != ControlStatus::Active {
                return reject(ControlRejection::InvalidTransition);
            }
            next.attempt = target.clone();
        }
    }
    let Some(revision) = current.revision.checked_add(1) else {
        return reject(ControlRejection::VersionExhausted);
    };
    next.revision = revision;
    if !matches!(command.action, ControlAction::ObserveAttempt { .. }) {
        let Some(generation) = current.control_generation.checked_add(1) else {
            return reject(ControlRejection::VersionExhausted);
        };
        next.control_generation = generation;
    }
    ControlReceipt {
        session_id: command.session_id.clone(),
        command_id: command.command_id.clone(),
        decision: ControlDecision::Accepted,
        state: next,
    }
}
