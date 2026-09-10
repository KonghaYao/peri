//! Prompt 执行结果、canonical 终态及安全错误投影。

use crate::{command::PromptStopReason, messages::BaseMessage};

// ─── ExecutionFailure（Agent→ACP 结果契约的 fatal failure DTO）────────────

/// 执行终止的稳定内部类别（ACP 边界据此选择协议错误码和 allowlist data）。
///
/// 仅区分客户端诊断需要的稳定类别；完整 `AgentError` 和 provider payload
/// 不得跨越本边界。
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
///   response / cause chain 意外跨层暴露；
/// - 只承载稳定类别 + 由 `AgentError::user_facing_message()` 生成的脱敏消息；
/// - `public_message` 保证非空（空输入 → 稳定 fallback 文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionFailure {
    /// 稳定内部类别。
    pub kind: ExecutionFailureKind,
    /// 非空、已脱敏且限长的用户可见消息。
    pub public_message: String,
    /// LLM HTTP 失败的状态码；其他类别为 `None`。
    pub http_status: Option<u16>,
}

impl ExecutionFailure {
    /// 构造 [`ExecutionFailureKind::Internal`] 类别，并保证 `public_message`
    /// 非空（空输入 → [`EXECUTION_FAILURE_FALLBACK_MESSAGE`]）。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ExecutionFailureKind::Internal, message, None)
    }

    /// 从 [`crate::error::AgentError`] 构造安全的失败投影。
    ///
    /// LLM 错误保留经过清洗和限长的原始含义；完整原文仍只存在于调用方的
    /// 受控诊断日志。HTTP status 作为独立 allowlist 字段保留。
    pub fn from_agent_error(error: &crate::error::AgentError) -> Self {
        match error {
            crate::error::AgentError::LlmHttpError { status, message } => Self::new(
                ExecutionFailureKind::LlmHttp,
                format!("LLM HTTP {status}: {}", redact_public_error(message)),
                Some(*status),
            ),
            crate::error::AgentError::LlmError(message) => Self::new(
                ExecutionFailureKind::Llm,
                format!("LLM error: {}", redact_public_error(message)),
                None,
            ),
            other => Self::internal(other.user_facing_message()),
        }
    }

    fn new(
        kind: ExecutionFailureKind,
        message: impl Into<String>,
        http_status: Option<u16>,
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
            http_status,
        }
    }

    /// 结果缺失时的防御性 failure（`PromptResult::default()` 等场景）：
    /// 缺失结果不能作为成功 `EndTurn` 继续交给 ACP。
    pub fn missing_result() -> Self {
        Self::internal(EXECUTION_FAILURE_FALLBACK_MESSAGE)
    }
}

/// 清洗可能进入用户可见边界的错误文本。
///
/// 遮蔽 bearer token、常见凭据赋值、URL userinfo/query，并按 Unicode 字符边界限长。
/// 该函数不是通用 secret scanner，原始 provider/script body 仍不得直接序列化。
pub fn sanitize_public_error(input: &str, max_chars: usize) -> String {
    let sanitized = truncate_chars(redact_public_error(input).trim(), max_chars);
    if sanitized.is_empty() && max_chars > 0 {
        truncate_chars(EXECUTION_FAILURE_FALLBACK_MESSAGE, max_chars)
    } else {
        sanitized
    }
}

/// 清洗可能进入客户端 wire 的 provider 错误文本。
///
/// 仅保留诊断原意，遮蔽 bearer token、常见凭据赋值和 URL query；最终输出由
/// [`ExecutionFailure::new`] 统一限制长度。该函数不是通用 secret scanner，
/// 原始 provider body 仍不得直接序列化。
fn redact_public_error(input: &str) -> String {
    redact_bearer_tokens(&redact_secret_fields(&redact_url_queries(input)))
}

fn redact_bearer_tokens(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut copied = 0;
    let mut index = 0;

    while index < bytes.len() {
        if !starts_with_ascii_case_insensitive(bytes, index, b"bearer")
            || (index > 0 && bytes[index - 1].is_ascii_alphanumeric())
        {
            index += input[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let mut value_start = index + b"bearer".len();
        if !bytes.get(value_start).is_some_and(u8::is_ascii_whitespace) {
            index = value_start;
            continue;
        }
        while bytes.get(value_start).is_some_and(u8::is_ascii_whitespace) {
            value_start += 1;
        }
        let value_end = bytes[value_start..]
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || b",;)}]\"'<>".contains(byte))
            .map_or(bytes.len(), |offset| value_start + offset);
        output.push_str(&input[copied..value_start]);
        output.push_str("[redacted]");
        copied = value_end;
        index = value_end;
    }

    output.push_str(&input[copied..]);
    output
}

fn url_scheme_end(bytes: &[u8], index: usize) -> Option<usize> {
    if !bytes.get(index)?.is_ascii_alphabetic() {
        return None;
    }
    let mut cursor = index + 1;
    while bytes
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    {
        cursor += 1;
    }
    (bytes.get(cursor..cursor + 3) == Some(b"://")).then_some(cursor)
}

fn redact_url_queries(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut copied = 0;
    let mut index = 0;

    while index < bytes.len() {
        let Some(scheme_end) = url_scheme_end(bytes, index) else {
            index += input[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let end = bytes[index..]
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || b"\"'<>)]}".contains(byte))
            .map_or(bytes.len(), |offset| index + offset);
        let authority_start = scheme_end + 3;
        let authority_end = bytes[authority_start..end]
            .iter()
            .position(|byte| matches!(byte, b'/' | b'?' | b'#'))
            .map_or(end, |offset| authority_start + offset);
        if let Some(userinfo_offset) = bytes[authority_start..authority_end]
            .iter()
            .rposition(|byte| *byte == b'@')
        {
            let userinfo_end = authority_start + userinfo_offset;
            output.push_str(&input[copied..authority_start]);
            output.push_str("[redacted]@");
            copied = userinfo_end + 1;
        }
        if let Some(query_offset) = bytes[index..end].iter().position(|byte| *byte == b'?') {
            let query = index + query_offset;
            output.push_str(&input[copied..=query]);
            output.push_str("[redacted]");
            copied = end;
        }
        index = end;
    }

    output.push_str(&input[copied..]);
    output
}

fn redact_secret_fields(input: &str) -> String {
    const KEYS: &[&[u8]] = &[
        b"authorization",
        b"aws_access_key_id",
        b"api_key",
        b"apikey",
        b"client_secret",
        b"connection_string",
        b"credential",
        b"password",
        b"private_key",
        b"secret",
        b"token",
        b"key",
    ];

    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut copied = 0;
    let mut index = 0;

    while index < bytes.len() {
        let boundary_before =
            index == 0 || (!bytes[index - 1].is_ascii_alphanumeric() && bytes[index - 1] != b'_');
        let key = boundary_before
            .then(|| {
                KEYS.iter().find(|key| {
                    starts_with_ascii_case_insensitive(bytes, index, key)
                        && bytes
                            .get(index + key.len())
                            .is_none_or(|next| !next.is_ascii_alphanumeric() && *next != b'_')
                })
            })
            .flatten();
        let Some(key) = key else {
            index += input[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        };

        let mut delimiter = index + key.len();
        if matches!(bytes.get(delimiter), Some(b'\'' | b'"'))
            && index > 0
            && bytes[index - 1] == bytes[delimiter]
        {
            delimiter += 1;
        }
        while bytes.get(delimiter).is_some_and(u8::is_ascii_whitespace) {
            delimiter += 1;
        }
        if !matches!(bytes.get(delimiter), Some(b':' | b'=')) {
            index += key.len();
            continue;
        }

        let mut value_start = delimiter + 1;
        while bytes.get(value_start).is_some_and(u8::is_ascii_whitespace) {
            value_start += 1;
        }
        if key.eq_ignore_ascii_case(b"authorization")
            && starts_with_ascii_case_insensitive(bytes, value_start, b"bearer")
        {
            value_start += b"bearer".len();
            while bytes.get(value_start).is_some_and(u8::is_ascii_whitespace) {
                value_start += 1;
            }
        }
        let quote = bytes
            .get(value_start)
            .copied()
            .filter(|byte| matches!(byte, b'\'' | b'"'));
        if quote.is_some() {
            value_start += 1;
        }
        if value_start >= bytes.len() {
            break;
        }

        let value_end = if let Some(quote) = quote {
            bytes[value_start..]
                .iter()
                .position(|byte| *byte == quote)
                .map_or(bytes.len(), |offset| value_start + offset)
        } else {
            bytes[value_start..]
                .iter()
                .position(|byte| byte.is_ascii_whitespace() || b",;)}]".contains(byte))
                .map_or(bytes.len(), |offset| value_start + offset)
        };

        output.push_str(&input[copied..value_start]);
        output.push_str("[redacted]");
        copied = value_end;
        index = value_end;
    }

    output.push_str(&input[copied..]);
    output
}

fn starts_with_ascii_case_insensitive(input: &[u8], index: usize, needle: &[u8]) -> bool {
    input
        .get(index..index + needle.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(needle))
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
    /// 以协议 error 结束，见 spec/issues/2026-08-18-acp-error-handler.md）。
    pub failure: Option<ExecutionFailure>,
    /// 持久化状态无法回滚或验证，宿主必须使 session 不可继续。
    pub persistence_inconsistent: bool,
    /// 本轮是否发生 Full Compact 提交并替换了先前的可见历史。
    pub history_replaced_by_compaction: bool,
    /// 执行期间收集的 recall 项（供下一轮注入）。
    pub recall_items: Vec<String>,
}

impl Default for PromptResult {
    /// 防御性回退（结果缺失 / 未执行时使用）：空失败结果。
    ///
    /// 结果缺失必须表达为安全的 fatal failure，不能作为成功 `EndTurn`
    /// 继续交给 ACP。
    fn default() -> Self {
        Self {
            persisted_payloads: Vec::new(),
            messages: Vec::new(),
            ok: false,
            stop_reason: PromptStopReason::EndTurn,
            history_replaced_by_compaction: false,
            persistence_inconsistent: false,
            recall_items: Vec::new(),
            failure: Some(ExecutionFailure::missing_result()),
        }
    }
}
