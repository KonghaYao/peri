//! Session 层契约类型（自 peri-agent 迁入；`peri-agent::session::{queue,turn,runtime}`
//! 与 `peri-agent::agent::session::{inbox,cron_owner}` 保留 re-export 保兼容）。
//!
//! 归位说明（§0 兜底：接口契约归 peri-acp-types）：MQ 消息管理、turn 身份、
//! inbox 唤醒、cron 触发桥、Agent 运行时注册表条目是跨层接口契约——
//! Agent 层持有实现与执行权，ACP / middlewares 只依赖本层契约类型。

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::command::PromptStopReason;
use crate::command_registry::CommandRegistry;
use crate::mcp_skills::McpSkillRegistry;
use crate::messages::BaseMessage;
use crate::thread::{CancelPolicy, ThreadId};

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
    /// 执行后的消息历史。
    pub messages: Vec<BaseMessage>,
    /// 是否执行成功。
    pub ok: bool,
    /// 执行停止原因。
    pub stop_reason: PromptStopReason,
    /// 致命执行失败（None = 正常终止 / 用户取消 / 最大轮数；Some = turn 应
    /// 以协议 error 结束，见 spec/issues/2026-08-18-acp-error-handler.md）。
    pub failure: Option<ExecutionFailure>,
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
            messages: Vec::new(),
            ok: false,
            stop_reason: PromptStopReason::EndTurn,
            history_replaced_by_compaction: false,
            recall_items: Vec::new(),
            failure: Some(ExecutionFailure::missing_result()),
        }
    }
}

// ─── TurnId ──────────────────────────────────────────────────────────────────

/// Turn 唯一标识符 — UUID v7（时间有序）
///
/// 作为一次 turn 内所有事件的统一纽带。从 LlmCallStart 到 TurnCompleted 全程一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(uuid::Uuid);

impl TurnId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl Default for TurnId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ─── MessageKind ─────────────────────────────────────────────────────────────

/// 消息 Kind — 控制循环唤醒行为
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// 外部主动请求 — drain_all 消费，循环结束后到达同样激活
    Prompt,
    /// 延迟到达的结果 — drain_all 消费，循环结束后到达同样激活
    Defer,
    /// 通知性数据 — drain_all 消费，永不唤醒循环
    Info,
}

impl MessageKind {
    /// 是否能唤醒新 turn
    pub fn wakes_up(self) -> bool {
        matches!(self, Self::Prompt | Self::Defer)
    }
}

// ─── MessageSource ───────────────────────────────────────────────────────────

/// 消息来源 — 用于调试和事件追踪
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    /// 外部用户输入
    UserInput,
    /// SubAgent 完成
    SubAgentComplete,
    /// 后台 Shell 完成
    ShellComplete,
    /// Goal steering（中途纠正）
    GoalSteering,
    /// Todo steering（requireCompletion 续跑提醒）
    TodoSteering,
    /// Cron 定时触发
    CronTrigger,
    /// Stop hook feedback
    StopHookFeedback,
    /// Channel 消息（微信/Slack 等）
    ChannelMessage,
    /// Dynamic MCP lifecycle notification.
    DynamicMcpNotification,
    /// Hook 系统注入
    SystemInjected,
    /// 工具失败警告
    ToolFailureWarning,
    /// 工作流完成
    WorkflowComplete,
}

// ─── QueuedMessage ───────────────────────────────────────────────────────────

/// 一条待投递的消息（v2 富类型）
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    /// 消息 Kind（决定唤醒行为）
    pub kind: MessageKind,
    /// 消息来源
    pub source: MessageSource,
    /// 实际消息内容
    pub message: BaseMessage,
}

impl QueuedMessage {
    pub fn new(kind: MessageKind, source: MessageSource, message: BaseMessage) -> Self {
        Self {
            kind,
            source,
            message,
        }
    }

    /// 快速构造 Prompt 消息（用户输入）
    pub fn prompt(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Prompt, source, message)
    }

    /// 快速构造 Defer 消息（SubAgent/Cron/Channel/Workflow 延迟结果）
    pub fn defer(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Defer, source, message)
    }

    /// 快速构造 Info 消息（SystemReminder/Hook 注入，不唤醒循环）
    pub fn info(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Info, source, message)
    }
}

// ─── MessageQueue ────────────────────────────────────────────────────────────

/// 会话级临时收件箱（v2）
///
/// 内部用 `Arc<Mutex<VecDeque>>` 保证线程安全。`Notify` 用于异步等待新消息。
///
/// RCRA 循环中 Receive 阶段通过 [`drain_all`] 一次性消费全部三类消息；
/// 循环退出后通过 [`has_wake_up`] 检测是否需重新激活。
#[derive(Debug, Clone)]
pub struct MessageQueue {
    inner: Arc<Mutex<VecDeque<QueuedMessage>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageQueue {
    /// 创建空队列
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// 推入一条消息，唤醒等待者
    pub fn push(&self, msg: QueuedMessage) {
        {
            let mut inner = self.inner.lock();
            inner.push_back(msg);
        }
        self.notify.notify_one();
    }

    /// 批量推入消息；空列表为 no-op
    pub fn push_batch(&self, msgs: Vec<QueuedMessage>) {
        if msgs.is_empty() {
            return;
        }
        {
            let mut inner = self.inner.lock();
            inner.extend(msgs);
        }
        self.notify.notify_one();
    }

    /// 排空队列中的全部消息（Prompt + Info + Defer）
    ///
    /// RCRA 循环的 Receive 阶段调用，一次性消费全部类型。
    pub fn drain_all(&self) -> Vec<QueuedMessage> {
        let mut inner = self.inner.lock();
        let drained: Vec<_> = std::mem::take(&mut *inner).into();
        drop(inner);
        self.notify.notify_one();
        drained
    }

    /// 是否有能唤醒循环的消息（Prompt 或 Defer）
    pub fn has_wake_up(&self) -> bool {
        self.inner.lock().iter().any(|m| m.kind.wakes_up())
    }

    /// 队列中是否存在指定来源的 pending Defer（wake-able 延迟结果）。
    ///
    /// AsyncContinuation 用：`session/cancel` 时确认 SubAgentComplete Defer 是否
    /// 已入队（race 兜底——bg 完成通知可能已在 cancel 前置位前被 scheduler 跳过），
    /// continuation scheduler 在真正 dispatch 前确认 Defer 尚未被消费（跳过空跑）。
    /// 仅匹配 `MessageKind::Defer`：Prompt/Info 均不计入。
    pub fn has_pending_defer(&self, source: &MessageSource) -> bool {
        self.inner
            .lock()
            .iter()
            .any(|m| m.kind == MessageKind::Defer && &m.source == source)
    }

    /// 队列是否为空
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// 队列长度
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// 清空队列（rewind 操作时调用）
    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}

// ─── SessionInbox / InboxHandle ──────────────────────────────────────────────

/// Wraps the existing v2 MessageQueue with an async await-wake mechanism.
///
/// During ReAct loop, `stages/receive.rs` calls `drain_all`
/// to consume pending messages — no wake needed (loop is already spinning).
///
/// During IDLE (between ReAct loops), the ACP executor calls [`await_wake`](Self::await_wake)
/// which blocks until a new Prompt/Defer is enqueued, then the loop resumes.
pub struct SessionInbox {
    queue: Arc<MessageQueue>,
    /// Dedicated notify for await_wake — separate from queue's internal notify
    /// to avoid spurious wakeups when Info messages are pushed.
    wake: Arc<tokio::sync::Notify>,
}

impl SessionInbox {
    /// Create a new SessionInbox wrapping the given queue.
    ///
    /// The queue is typically the session-level shared instance passed through
    /// `Session::new_with_cancel_and_queue`.
    pub fn new(queue: Arc<MessageQueue>) -> Self {
        Self {
            queue,
            wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Block until the inbox has at least one wake-able message (Prompt or Defer).
    ///
    /// Called by ACP executor's `run_session_loop` when the previous iteration ends
    /// with `should_continue = false` (no more messages to process).
    ///
    /// ## Non-destructive
    ///
    /// This method does NOT drain any messages. The actual consumption happens in
    /// `stages/receive.rs` via `drain_all`; `drain_for_receive` and `drain_for_end`
    /// remain available for external flush callers.
    ///
    /// ## Spurious wakeup guard
    ///
    /// After waking, we re-check `has_wake_up()`. If only Info messages arrived
    /// (which don't wake the loop), we go back to waiting. This prevents the executor
    /// from spinning on Info-only notifications.
    pub async fn await_wake(&self) {
        // Fast path: if already pending, return immediately
        if self.queue.has_wake_up() {
            return;
        }
        loop {
            self.wake.notified().await;
            // Guard against spurious wakeups: only wake on Prompt/Defer
            if self.queue.has_wake_up() {
                return;
            }
        }
    }

    /// Get a cloneable handle for producers.
    ///
    /// Producers (cron owner, channel owner, async router for bg_results, etc.)
    /// use this handle to push messages and wake the idle executor.
    pub fn handle(&self) -> InboxHandle {
        InboxHandle {
            queue: Arc::clone(&self.queue),
            wake: Arc::clone(&self.wake),
        }
    }

    /// Access the underlying MessageQueue (read-only reference).
    ///
    /// Used by stages that need to drain (e.g., `StageContext` construction).
    pub fn queue(&self) -> &MessageQueue {
        &self.queue
    }
}

impl std::fmt::Debug for SessionInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionInbox")
            .field("queue_len", &self.queue.len())
            .finish()
    }
}

/// Cloneable handle for pushing messages into the SessionInbox.
///
/// Producers (cron_owner, channel_owner, async_router for bg_results) hold this
/// handle to push messages and wake the idle executor. The handle is `Send + Sync`
/// and cheaply cloneable — safe to store in long-lived components.
///
/// TUI should NOT have access to this handle.
#[derive(Clone)]
pub struct InboxHandle {
    queue: Arc<MessageQueue>,
    wake: Arc<tokio::sync::Notify>,
}

impl InboxHandle {
    /// Push a Prompt message (user input or external request) and wake the executor.
    ///
    /// Prompt messages are consumed by `drain_all` during the Receive stage
    /// and wake the loop.
    pub fn push_prompt(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::prompt(source, message));
        self.wake.notify_one();
    }

    /// Push a Defer message (SubAgent complete, Cron trigger, bg result) and wake.
    ///
    /// In RCRA, Defer messages are consumed by `drain_all` during the Receive stage.
    /// They are also detectable via `drain_for_end` for external callers.
    pub fn push_defer(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::defer(source, message));
        self.wake.notify_one();
    }

    /// Push an Info message (system reminder, hook injection) — does NOT wake.
    ///
    /// Info messages are consumed by `drain_all` (in the loop) or `drain_for_receive`
    /// (external flush paths), but never wake the loop.
    /// They must be carried out by a Prompt message arriving later.
    pub fn push_info(&self, source: MessageSource, message: BaseMessage) {
        // Intentionally no wake.notify_one() — Info does not wake the loop
        self.queue.push(QueuedMessage::info(source, message));
    }

    /// Push an arbitrary QueuedMessage and conditionally wake.
    ///
    /// Wakes only if the message kind is Prompt or Defer (i.e., `kind.wakes_up()`).
    pub fn push(&self, msg: QueuedMessage) {
        let should_wake = msg.kind.wakes_up();
        self.queue.push(msg);
        if should_wake {
            self.wake.notify_one();
        }
    }

    /// Batch push messages; wakes once if any message is wake-able.
    pub fn push_batch(&self, msgs: Vec<QueuedMessage>) {
        if msgs.is_empty() {
            return;
        }
        let should_wake = msgs.iter().any(|m| m.kind.wakes_up());
        self.queue.push_batch(msgs);
        if should_wake {
            self.wake.notify_one();
        }
    }
}

impl std::fmt::Debug for InboxHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboxHandle")
            .field("queue_len", &self.queue.len())
            .finish()
    }
}

// ─── CronOwner ───────────────────────────────────────────────────────────────

/// Agent-owned cron evaluation bridge。
///
/// Spawns a tokio task that receives trigger prompts from the channel and
/// pushes each prompt into the inbox as a Defer + `CronTrigger` source。
///
/// 循环依赖规避：本模块不 import `CronScheduler` / `CronTrigger`
/// （peri-middlewares 类型），只接收 `UnboundedReceiver<String>`——
/// 从 `CronTrigger.prompt` 到本通道的桥接在装配点（peri-acp host）完成。
pub struct CronOwner {
    /// Handle to the spawned trigger-forwarding task.
    /// `None` before [`start`](Self::start) is called.
    handle_task: Option<tokio::task::JoinHandle<()>>,
}

impl CronOwner {
    /// Create a new (not yet started) CronOwner.
    pub fn new() -> Self {
        Self { handle_task: None }
    }

    /// Spawn the trigger-forwarding loop.
    ///
    /// Receives prompt strings from `trigger_rx` and pushes each one into
    /// the inbox as `QueuedMessage::defer(MessageSource::CronTrigger, ...)`.
    ///
    /// The loop terminates when either:
    /// - `shutdown` is cancelled (session tear-down), or
    /// - `trigger_rx` is closed (scheduler dropped).
    ///
    /// # Parameters
    ///
    /// - `trigger_rx`: Unbounded receiver of prompt strings. Each received
    ///   string is the prompt from a fired `CronTrigger`.
    /// - `inbox`: Cloneable handle to the session inbox.
    /// - `shutdown`: Cancellation token tied to the session lifetime (Arc-shared clone).
    pub fn start(
        &mut self,
        mut trigger_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
        inbox: InboxHandle,
        shutdown: Arc<tokio_util::sync::CancellationToken>,
    ) {
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => {
                        tracing::debug!("cron_owner: shutdown signal received, stopping");
                        break;
                    }
                    prompt = trigger_rx.recv() => {
                        match prompt {
                            Some(prompt) => {
                                let message = BaseMessage::human(
                                    crate::messages::MessageContent::text(format!(
                                        "<goal-message>Cron triggered: {}</goal-message>",
                                        prompt
                                    )),
                                );
                                inbox.push(QueuedMessage::defer(
                                    MessageSource::CronTrigger,
                                    message,
                                ));
                                tracing::debug!(prompt = %prompt, "cron_owner: trigger pushed to inbox");
                            }
                            None => {
                                // trigger_rx closed (scheduler dropped)
                                tracing::debug!("cron_owner: trigger_rx closed, stopping");
                                break;
                            }
                        }
                    }
                }
            }
        });
        self.handle_task = Some(handle);
    }

    /// Abort the background task if running.
    ///
    /// Called during session tear-down to ensure clean shutdown even if the
    /// cancellation token has not yet fired.
    pub fn shutdown(&mut self) {
        if let Some(handle) = self.handle_task.take() {
            handle.abort();
        }
    }
}

impl Default for CronOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CronOwner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for CronOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronOwner")
            .field("running", &self.handle_task.is_some())
            .finish()
    }
}

// ─── AgentRuntime（注册表条目 + cancel 判定） ────────────────────────────────

/// 运行时 agent 实例（子 agent 取消判定与终止执行的载体）。
///
/// cancel 最终执行权归 Agent 层（§2/§9）：本类型是跨层注册表条目契约
/// （ACP `AcpSession.active_agents` 持有），判定函数为纯函数、无层依赖。
pub struct AgentRuntime {
    pub thread_id: ThreadId,
    pub cancel_token: tokio_util::sync::CancellationToken,
    pub cancel_policy: CancelPolicy,
    pub status: crate::thread::AgentStatus,
}

impl AgentRuntime {
    pub fn new(thread_id: ThreadId, cancel_policy: CancelPolicy) -> Self {
        Self {
            thread_id,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            cancel_policy,
            status: crate::thread::AgentStatus::Active,
        }
    }
}

/// cancel 判定（Cascade/Independent）与终止执行：取消所有 Cascade policy 的
/// 同步子 agent（跟随父 agent 取消）。Independent（bg）子 agent 不受影响，
/// 仅跟随 session 根取消。
pub fn cancel_cascade_agents<'a>(runtimes: impl IntoIterator<Item = &'a AgentRuntime>) {
    for runtime in runtimes {
        if runtime.cancel_policy == CancelPolicy::Cascade {
            runtime.cancel_token.cancel();
        }
    }
}

/// 取消所有 agent（session 结束 / close_session 时）。
pub fn cancel_all_agents<'a>(runtimes: impl IntoIterator<Item = &'a AgentRuntime>) {
    for runtime in runtimes {
        runtime.cancel_token.cancel();
    }
}

/// 便捷入口：按 `thread_id -> AgentRuntime` 注册表执行 cascade 判定。
pub fn cancel_cascade_in<'a>(
    runtimes: impl IntoIterator<Item = &'a HashMap<ThreadId, AgentRuntime>>,
) {
    for map in runtimes {
        cancel_cascade_agents(map.values());
    }
}

/// 便捷入口：按 `thread_id -> AgentRuntime` 注册表取消全部。
pub fn cancel_all_in<'a>(runtimes: impl IntoIterator<Item = &'a HashMap<ThreadId, AgentRuntime>>) {
    for map in runtimes {
        cancel_all_agents(map.values());
    }
}

// ─── SessionAccessPort（L5：executor 对 ACP SessionManager 的访问端口）──────

/// L5：`run_session_loop` 会话编排对 ACP `SessionManager` 的依赖端口。
///
/// 依赖反转（§0）：executor 迁入 peri-agent 后不再引用 ACP `SessionManager`
/// 类型，改为经本端口访问会话级状态（v2 MessageQueue / inbox / task manager /
/// goal / 子 agent 注册表 / cron bridge）。
/// ACP 侧 `SessionManager` 实现本端口；print mode / 测试等无 session 场景
/// 为 `None`（调用方保持原 None 语义，仅读路径可用时生效）。
pub trait SessionAccessPort: Send + Sync {
    /// 会话级共享 v2 MessageQueue（`AcpSession.v2_message_queue`）。
    /// 返回 clone（内部 Arc 共享，语义同 `SessionManager::v2_queue_for`）。
    fn v2_message_queue(&self, session_id: &str) -> Option<MessageQueue>;

    /// 会话级 SessionInbox（await-wake wrapper；lazy-init 语义由实现方保证）。
    fn session_inbox(&self, session_id: &str) -> Option<Arc<SessionInbox>>;

    /// 会话级 idle-suspended 标志（共享 Arc，executor 在 await_wake 挂起期间
    /// 置 true、醒来/取消时复位）。
    ///
    /// 宿主 `dispatch_prompt_turn` 读取此标志决定"注入 vs 排队"：turn 挂起时
    /// 用户新 prompt 直接注入 inbox（Prompt + wake）让挂起的 loop 立即醒来，
    /// 而不是在 per-session prompt lock 上阻塞至当前 turn 完成（bg 任务可能
    /// 长达数分钟，阻塞会让用户输入"石沉大海"）。
    fn idle_suspended_flag(&self, session_id: &str) -> Option<Arc<AtomicBool>>;

    /// 会话级后台任务管理器（`AcpSession.task_manager`）。
    fn task_manager(&self, session_id: &str) -> Option<Arc<dyn crate::tasks::TaskManager>>;

    /// 会话级 GoalController（`AcpSession.goal_state`）。
    fn goal_controller(&self, session_id: &str) -> Option<Arc<dyn crate::goal::GoalController>>;

    /// 构造子 agent runtime 注册闭包（`AcpSession.active_agents` insert）。
    /// 返回 None 表示无注册能力（print mode / session 不存在）。
    fn register_runtime(&self, session_id: &str) -> Option<crate::frozen::RegisterRuntimeFn>;

    /// 构造子 agent runtime 注销闭包（`AcpSession.active_agents` remove）。
    fn deregister_runtime(&self, session_id: &str) -> Option<crate::frozen::DeregisterRuntimeFn>;

    /// cancel cascade 子 agent（Cascade 判定归 Agent 层契约，本端口仅定位）。
    fn cancel_cascade_children(&self, session_id: &str);

    /// 确保 session 级 cron bridge 已启动（lazy-init，幂等；见
    /// `SessionManager::cron_bridge_for`）。
    fn cron_bridge_for(&self, session_id: &str) -> bool;

    /// 确保 session 级 MCP 订阅 inbox 已注册（lazy-init，幂等；见
    /// `SessionManager::mcp_subscription_for`）。
    ///
    /// 默认实现返回 false（print mode / 未装配端口时安全 no-op）。
    fn mcp_subscription_for(&self, _session_id: &str) -> bool {
        false
    }

    /// Bind the existing session inbox as the checked Dynamic MCP notification target.
    fn dynamic_mcp_notifications_for(&self, _session_id: &str) -> bool {
        false
    }

    /// 会话级 MCP skill 远端注册表（AcpSession 持有；发现任务写入，
    /// Skills 侧读取合并）。
    ///
    /// 默认实现返回 None（print mode / 未装配端口时安全 no-op）。
    fn mcp_skill_registry(&self, _session_id: &str) -> Option<Arc<McpSkillRegistry>> {
        None
    }

    /// 会话级命令注册表（AcpSession 持有；命令面动态注入——MCP/插件发现
    /// 结果经注册表写入，投影经 snapshot 下发）。
    ///
    /// 默认实现返回 None（print mode / 未装配端口时安全 no-op）。
    fn command_registry(&self, _session_id: &str) -> Option<Arc<CommandRegistry>> {
        None
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
