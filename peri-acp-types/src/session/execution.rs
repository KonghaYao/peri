//! Prompt 执行结果、canonical 终态及有界错误投影。

use crate::{command::PromptStopReason, messages::BaseMessage};

// ─── ExecutionFailure（Agent→ACP 结果契约的 fatal failure DTO）────────────

/// 执行终止的稳定内部类别（ACP 边界据此选择协议错误码和 allowlist data）。
///
/// 仅区分客户端诊断需要的稳定类别；完整 `AgentError` 和 provider payload
/// 经显式投影跨越本边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionFailureKind {
    /// 非 LLM 的内部执行失败。
    Internal,
    /// 无 HTTP status 的 LLM/provider 失败。
    Llm,
    /// 带 HTTP status 的 LLM/provider 失败。
    LlmHttp,
}

/// Langfuse 等进程内观测消费者使用的 canonical turn 终态。
///
/// 该 DTO 不参与 wire 序列化；fatal 分支只携带 [`ExecutionFailure`] 的安全窄投影，
/// 避免观测侧从可丢弃事件或错误字符串重新推断终态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnTelemetryOutcome {
    Completed,
    Stopped { reason: PromptStopReason },
    Failed { failure: ExecutionFailure },
}

impl TurnTelemetryOutcome {
    pub fn from_result(stop_reason: PromptStopReason, failure: Option<ExecutionFailure>) -> Self {
        if let Some(failure) = failure {
            Self::Failed { failure }
        } else if stop_reason == PromptStopReason::EndTurn {
            Self::Completed
        } else {
            Self::Stopped {
                reason: stop_reason,
            }
        }
    }
}

impl ExecutionFailureKind {
    /// JSON-RPC error `data.kind` 的稳定 wire 名称。
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Llm => "llm",
            Self::LlmHttp => "llm_http",
        }
    }
}

/// 结果缺失 / 空 message 时的稳定非空 fallback 文案（脱敏、无内部细节）。
pub const EXECUTION_FAILURE_FALLBACK_MESSAGE: &str =
    "An internal error occurred. Check logs for details.";

/// Agent→ACP 结果边界的窄 fatal failure DTO。
///
/// 设计约束（spec D1/D5）：
/// - **非 serde**：不参与 wire 序列化，阻止完整 `AgentError` / provider
///   response 的边界适配显式完成；
/// - 只承载稳定类别 + 由 `AgentError::user_facing_message()` 生成的原始诊断消息；
/// - `public_message` 保证非空（空输入 → 稳定 fallback 文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionFailure {
    /// 稳定内部类别。
    pub kind: ExecutionFailureKind,
    /// 非空、保留原意且限长的用户可见消息。
    pub public_message: String,
    pub error_category: Option<String>,
    pub causes: Vec<String>,
    /// LLM HTTP 失败的状态码；其他类别为 `None`。
    pub http_status: Option<u16>,
    /// Optional allowlisted model facts. ACP serializes these through an
    /// explicit host projection; this DTO itself is not serde.
    pub diagnostic: Option<peri_model::ModelErrorDiagnostic>,
}

impl ExecutionFailure {
    /// 构造 [`ExecutionFailureKind::Internal`] 类别，并保证 `public_message`
    /// 非空（空输入 → [`EXECUTION_FAILURE_FALLBACK_MESSAGE`]）。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ExecutionFailureKind::Internal, message, None, None)
    }

    /// 从 [`crate::error::AgentError`] 构造安全的失败投影。
    ///
    /// LLM 错误保留限长后的原始含义；完整原文仍只存在于调用方的
    /// 受控诊断日志。HTTP status 作为独立 allowlist 字段保留。
    pub fn from_agent_error(error: &crate::error::AgentError) -> Self {
        let mut failure = match error {
            crate::error::AgentError::WorkBudgetExhausted { .. } => {
                Self::internal(error.user_facing_message())
            }
            crate::error::AgentError::LlmHttpError { status, message } => Self::new(
                ExecutionFailureKind::LlmHttp,
                format!("LLM HTTP {status}: {}", message),
                Some(*status),
                None,
            ),
            crate::error::AgentError::LlmError(message) => Self::new(
                ExecutionFailureKind::Llm,
                format!("LLM error: {}", message),
                None,
                None,
            ),
            crate::error::AgentError::CompactIncompleteResponse { .. } => Self::new(
                ExecutionFailureKind::Llm,
                error.user_facing_message(),
                None,
                None,
            ),
            crate::error::AgentError::ModelError(source)
            | crate::error::AgentError::StreamRecoveryExhausted { source, .. } => {
                let diagnostic = source.diagnostic();
                let kind = if diagnostic.status().is_some() {
                    ExecutionFailureKind::LlmHttp
                } else {
                    ExecutionFailureKind::Llm
                };
                Self::new(
                    kind,
                    error.user_facing_message(),
                    diagnostic.status(),
                    Some(diagnostic),
                )
            }
            other => Self::internal(other.user_facing_message()),
        };
        failure.error_category = Some(error.category_name().to_owned());
        failure.causes = crate::error::bounded_error_causes(error.cause_chain());
        failure
    }

    fn new(
        kind: ExecutionFailureKind,
        message: impl Into<String>,
        http_status: Option<u16>,
        diagnostic: Option<peri_model::ModelErrorDiagnostic>,
    ) -> Self {
        let message = message.into();
        let public_message = if message.trim().is_empty() {
            EXECUTION_FAILURE_FALLBACK_MESSAGE.to_string()
        } else {
            truncate_chars(message.trim(), 2_000)
        };
        Self {
            kind,
            public_message,
            error_category: None,
            causes: Vec::new(),
            http_status,
            diagnostic,
        }
    }

    /// 结果缺失时的防御性 failure（`PromptResult::default()` 等场景）：
    /// 缺失结果不能作为成功 `EndTurn` 继续交给 ACP。
    pub fn missing_result() -> Self {
        Self::internal(EXECUTION_FAILURE_FALLBACK_MESSAGE)
    }
}

pub fn bounded_error_message(input: &str, max_chars: usize) -> String {
    let message = truncate_chars(input.trim(), max_chars);
    if message.is_empty() && max_chars > 0 {
        truncate_chars(EXECUTION_FAILURE_FALLBACK_MESSAGE, max_chars)
    } else {
        message
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

// ─── PromptResult（L5：自 peri-acp host/exec/executor.rs 契约化）────────────

/// 单轮 prompt 执行结果（ACP 协议面 / 执行薄壳消费；Agent 层命令执行体与
/// 执行句柄经本类型回传）。
pub struct PromptResult {
    /// Canonical transcript snapshot captured after Agent persistence barrier.
    pub persisted_payloads: Vec<crate::store::PersistedPayload>,
    /// 执行后的消息历史。
    pub messages: Vec<BaseMessage>,
    /// 是否执行成功。
    pub ok: bool,
    /// 执行停止原因。
    pub stop_reason: PromptStopReason,
    /// 致命执行失败（None = 正常终止 / 用户取消 / 最大轮数；Some = turn 应
    /// 以协议 error 结束，见 spec/history/2026-08.md 2026-08-18 条目）。
    pub failure: Option<ExecutionFailure>,
    /// 没有可验证的 canonical snapshot，宿主必须移除热 session 并要求冷加载。
    pub persistence_inconsistent: bool,
    /// 本轮是否发生 Full Compact 提交并替换了先前的可见历史。
    pub history_replaced_by_compaction: bool,
    /// 执行期间收集的 recall 项（供下一轮注入）。
    pub recall_items: Vec<String>,
    /// turn 退出时仍未结算的后台任务数（§7.3 有界等待摘要）。
    /// `0` = 无未结算任务，宿主不在 ACP 响应上附加 pending 标记。
    pub pending_tasks: u32,
}

impl Default for PromptResult {
    /// 防御性回退（结果缺失 / 未执行时使用）：空失败结果。
    ///
    /// 结果缺失必须表达为安全的 fatal failure；未知持久化状态要求冷加载，
    /// 不能让空 payload 覆盖 host 的历史快照。
    fn default() -> Self {
        Self {
            persisted_payloads: Vec::new(),
            messages: Vec::new(),
            ok: false,
            stop_reason: PromptStopReason::EndTurn,
            history_replaced_by_compaction: false,
            persistence_inconsistent: true,
            recall_items: Vec::new(),
            pending_tasks: 0,
            failure: Some(ExecutionFailure::missing_result()),
        }
    }
}

#[cfg(test)]
#[path = "execution_test.rs"]
mod tests;
