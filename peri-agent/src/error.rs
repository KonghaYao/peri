use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Max iterations exceeded ({0})")]
    MaxIterationsExceeded(usize),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Tool execution failed: {tool} - {reason}")]
    ToolExecutionFailed { tool: String, reason: String },

    #[error("LLM error: {0}")]
    LlmError(String),

    #[error("LLM HTTP 错误 ({status}): {message}")]
    LlmHttpError { status: u16, message: String },

    #[error("Middleware error: {middleware} - {reason}")]
    MiddlewareError { middleware: String, reason: String },

    #[error("Tool rejected: {tool} - {reason}")]
    ToolRejected { tool: String, reason: String },

    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// 用户主动中断（Ctrl+C）
    #[error("Interrupted by user")]
    Interrupted,

    #[error("Full Compact requires LLM instance")]
    CompactNoLlm,

    #[error("Full Compact failed: LLM returned empty summary")]
    CompactEmptyResponse,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type AgentResult<T> = Result<T, AgentError>;

impl AgentError {
    /// 返回用户可见的错误描述（脱敏后的消息）
    /// 对 Other/LlmError/LlmHttpError/SerializationError 返回通用描述
    pub fn user_facing_message(&self) -> String {
        match self {
            Self::Other(_) => "An internal error occurred. Check logs for details.".to_string(),
            Self::LlmError(_) => "LLM call failed. Check logs for details.".to_string(),
            Self::LlmHttpError { status, .. } => {
                format!("LLM HTTP error ({status})")
            }
            Self::SerializationError(_) => {
                "Serialization error. Check logs for details.".to_string()
            }
            other => other.to_string(),
        }
    }
}
