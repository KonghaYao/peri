use serde::{Deserialize, Serialize};

use super::TurnId;
use crate::identity::AttemptId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionBinding {
    pub turn_id: TurnId,
    pub attempt_id: AttemptId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRequirement {
    Required,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "execution", rename_all = "snake_case")]
pub enum MessageActivation {
    Passive,
    ContinueCurrentRun(ExecutionBinding),
    EnsureProcessing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePolicy {
    pub requirement: MessageRequirement,
    pub activation: MessageActivation,
    pub model_visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageDisposition {
    Process,
    ProjectOnly,
    Suppressed,
}

impl MessagePolicy {
    pub fn ensure_processing() -> Self {
        Self {
            requirement: MessageRequirement::Required,
            activation: MessageActivation::EnsureProcessing,
            model_visible: true,
        }
    }

    pub fn passive() -> Self {
        Self {
            requirement: MessageRequirement::Optional,
            activation: MessageActivation::Passive,
            model_visible: true,
        }
    }

    pub fn continue_current_run(execution: ExecutionBinding) -> Self {
        Self {
            requirement: MessageRequirement::Required,
            activation: MessageActivation::ContinueCurrentRun(execution),
            model_visible: true,
        }
    }

    pub fn disposition(&self, execution: &ExecutionBinding) -> MessageDisposition {
        if let MessageActivation::ContinueCurrentRun(bound) = &self.activation {
            if bound != execution {
                return MessageDisposition::Suppressed;
            }
        }
        if self.requirement == MessageRequirement::Optional
            || !self.model_visible
            || self.activation == MessageActivation::Passive
        {
            MessageDisposition::ProjectOnly
        } else {
            MessageDisposition::Process
        }
    }

    pub fn ensures_processing(&self) -> bool {
        self.requirement == MessageRequirement::Required
            && self.model_visible
            && self.activation == MessageActivation::EnsureProcessing
    }

    pub fn notifies_execution(&self) -> bool {
        self.requirement == MessageRequirement::Required
            && self.model_visible
            && self.activation != MessageActivation::Passive
    }
}
