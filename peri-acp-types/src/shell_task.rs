//! Workspace Shell evidence retained beside a standard MCP cancelled task.
//!
//! MCP `CancelledTask` has no result payload. The workspace keeps its actual
//! executor result in `tasks/get` metadata after process cleanup completes.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::BackgroundTaskResult;

pub const SHELL_TASK_RESULT_META_KEY: &str = "peri/shell-result";
const SHELL_TASK_RESULT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellTaskResultMeta {
    pub version: u32,
    pub result: BackgroundTaskResult,
}

#[derive(Debug, Error)]
pub enum ShellTaskResultError {
    #[error("unsupported shell task result version: {0}")]
    UnsupportedVersion(u32),
    #[error("shell task result identity mismatch: expected {expected}, received {actual}")]
    IdentityMismatch { expected: String, actual: String },
    #[error("cancelled shell task has incompatible executor result: agent={agent}, state={state}")]
    OutcomeMismatch { agent: String, state: String },
    #[error("cancelled shell task lacks typed output evidence")]
    MissingOutput,
}

impl ShellTaskResultMeta {
    pub fn new(result: BackgroundTaskResult) -> Self {
        Self {
            version: SHELL_TASK_RESULT_VERSION,
            result,
        }
    }

    /// Only the matching workspace cancellation may consume these file references.
    pub fn into_cancelled_result(
        self,
        raw_task_id: &str,
    ) -> Result<BackgroundTaskResult, ShellTaskResultError> {
        if self.version != SHELL_TASK_RESULT_VERSION {
            return Err(ShellTaskResultError::UnsupportedVersion(self.version));
        }
        if self.result.task_id != raw_task_id {
            return Err(ShellTaskResultError::IdentityMismatch {
                expected: raw_task_id.to_owned(),
                actual: self.result.task_id,
            });
        }
        let state = self.result.outcome_label();
        if self.result.agent_name != "bg-shell" || state != "cancelled" {
            return Err(ShellTaskResultError::OutcomeMismatch {
                agent: self.result.agent_name,
                state: state.to_owned(),
            });
        }
        if self.result.shell_output.is_none() {
            return Err(ShellTaskResultError::MissingOutput);
        }
        Ok(self.result)
    }
}
