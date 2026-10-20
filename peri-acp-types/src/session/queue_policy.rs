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
    /// 只对绑定执行生效的控制意图（如 hook 的显式停止）：不驱动语义续跑，
    /// 新执行中按 `Suppressed` 处理，避免旧 Stop 终止新 run。
    StopCurrentRun(ExecutionBinding),
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

    /// 绑定当前执行的停止意图：同一执行仍经 Receive 唯一退出口生效；
    /// 其他执行中视为过期控制消息（`Suppressed`），不产生推理义务。
    pub fn stop_current_run(execution: ExecutionBinding) -> Self {
        Self {
            requirement: MessageRequirement::Optional,
            activation: MessageActivation::StopCurrentRun(execution),
            model_visible: true,
        }
    }

    pub fn disposition(&self, execution: &ExecutionBinding) -> MessageDisposition {
        match &self.activation {
            MessageActivation::ContinueCurrentRun(bound)
            | MessageActivation::StopCurrentRun(bound) => {
                if bound != execution {
                    return MessageDisposition::Suppressed;
                }
            }
            MessageActivation::Passive | MessageActivation::EnsureProcessing => {}
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
