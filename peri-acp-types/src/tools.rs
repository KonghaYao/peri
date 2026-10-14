//! 工具契约类型（自 peri-agent 迁入，`peri-agent::tools` 保留 re-export）。

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::interaction::UnansweredCause;
use crate::messages::BaseMessage;
use crate::tool_outcome::{RawToolResult, RemoteToolError};

/// Stable execution state carried alongside a tool's bounded display text.
///
/// `None` on [`ToolOutput::execution`] means that the legacy tool contract did
/// not provide execution evidence. Callers must preserve that uncertainty
/// rather than treating an arbitrary string result as a completed command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Unknown,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
    /// The deadline elapsed but the process was promoted and is still alive.
    RunningAfterTimeout,
    Running,
}

impl ToolExecutionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::RunningAfterTimeout => "running_after_timeout",
            Self::Running => "running",
        }
    }
}

/// Typed facts about one tool execution. The text remains a bounded model/UI
/// projection; these fields are the durable evidence used by persistence and
/// offline analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionEvidence {
    pub status: ToolExecutionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
    /// Whether the bounded text is a projection of a larger output. A missing
    /// `output_ref` then means persistence failed (or was unavailable), not
    /// that the output was complete.
    #[serde(default)]
    pub output_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl ToolExecutionEvidence {
    /// Compact bounded display summary derived from the same typed facts that
    /// are persisted. Consumers must not infer these fields from tool text.
    pub fn render_summary(&self) -> String {
        let mut summary = format!("[Execution status: {}", self.status.as_str());
        if let Some(code) = self.exit_code {
            summary.push_str(&format!(", exit_code: {code}"));
        }
        if let Some(ref output_ref) = self.output_ref {
            summary.push_str(&format!(", output_ref: {output_ref}"));
        } else if self.output_truncated {
            summary.push_str(", output_ref: unavailable");
        }
        if let Some(ref task_id) = self.task_id {
            summary.push_str(&format!(", task_id: {task_id}"));
        }
        summary.push(']');
        summary
    }

    /// Render the facts without truncating a status name. This fallback is
    /// used when the full labelled summary cannot fit in the display budget;
    /// the typed fields remain authoritative for facts omitted from the view.
    pub fn render_compact_summary(&self, limit: usize) -> String {
        if limit == 0 {
            return String::new();
        }

        let status = self.status.as_str();
        if status.chars().count() > limit {
            return "…".repeat(limit);
        }

        let mut compact = status.to_string();
        let mut append = |field: String| {
            let candidate = format!("{compact}, {field}");
            if candidate.chars().count() <= limit {
                compact = candidate;
            }
        };

        if let Some(code) = self.exit_code {
            append(format!("exit_code: {code}"));
        }
        if let Some(output_ref) = &self.output_ref {
            append(format!("output_ref: {output_ref}"));
        } else if self.output_truncated {
            append("output_ref: unavailable".to_string());
        }
        if let Some(task_id) = &self.task_id {
            append(format!("task_id: {task_id}"));
        }
        compact
    }
}

/// 工具结果的 typed 事实。
///
/// 三个事实来源必须分开，任何一方缺省都不得由另一方推断：
/// - [`ToolOutcome`]：工具/远端 MCP 自身报告的结果（真实 `isError` + raw 载荷）；
/// - [`ToolExecutionEvidence`]：本进程侧的执行状态（进程存活、退出码、落盘引用）；
/// - [`EffectiveToolError`]：transport/权限/取消等协议层失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolOutcome {
    /// 未报告：工具没有提供结构化结果（legacy `BaseTool::invoke` 路径）。调用方
    /// 必须保留这份不确定性，不得读作成功、失败或完成。
    Unreported,
    /// 已报告工具自身的结果。
    Reported {
        /// 工具自身报告的错误标志（MCP `isError`）。与 transport/协议失败不同：
        /// 调用本身成功，但工具在带内报告失败。
        is_error: bool,
        /// 无损 raw 结果；`None` = 该工具没有 raw 载荷（本地工具）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw: Option<RawToolResult>,
    },
}

impl Default for ToolOutcome {
    /// 缺省即「未报告」：反序列化旧数据或缺失字段时不引入成功/失败的假设。
    fn default() -> Self {
        Self::Unreported
    }
}

impl ToolOutcome {
    /// 从无损 raw 结果构造；`is_error` 取 raw 的 `isError`，不额外推断。
    pub fn from_raw(raw: RawToolResult) -> Self {
        Self::Reported {
            is_error: raw.is_error(),
            raw: Some(raw),
        }
    }

    /// 工具如实报告结果但没有 raw 载荷（本地工具）。
    pub fn reported(is_error: bool) -> Self {
        Self::Reported {
            is_error,
            raw: None,
        }
    }

    /// 是否为 legacy 未报告状态。
    pub fn is_unreported(&self) -> bool {
        matches!(self, Self::Unreported)
    }

    /// `Some(true/false)` = 工具如实报告；`None` = 未报告，不得推断。
    pub fn is_error(&self) -> Option<bool> {
        match self {
            Self::Unreported => None,
            Self::Reported { is_error, .. } => Some(*is_error),
        }
    }

    /// 无损 raw 结果（若工具提供）。
    pub fn raw(&self) -> Option<&RawToolResult> {
        match self {
            Self::Unreported => None,
            Self::Reported { raw, .. } => raw.as_ref(),
        }
    }
}

/// Tool text plus optional execution evidence and typed result outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ToolExecutionEvidence>,
    /// 工具结果事实。缺省 [`ToolOutcome::Unreported`]：legacy 工具仍按不确定处理，
    /// 其序列化形状与新增该字段之前一致（不写入 `outcome`）。
    #[serde(default, skip_serializing_if = "ToolOutcome::is_unreported")]
    pub outcome: ToolOutcome,
}

impl ToolOutput {
    /// Legacy 契约：只有文本，执行状态与结果事实都未报告。
    pub fn from_legacy(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            execution: None,
            outcome: ToolOutcome::Unreported,
        }
    }

    /// 携带本进程执行证据构造。执行证据不是工具结果：`outcome` 仍为
    /// [`ToolOutcome::Unreported`]，需要报告结果的工具请用
    /// [`ToolOutput::with_outcome`] / [`ToolOutput::from_raw_result`]。
    pub fn with_execution(text: impl Into<String>, execution: ToolExecutionEvidence) -> Self {
        Self {
            text: text.into(),
            execution: Some(execution),
            outcome: ToolOutcome::Unreported,
        }
    }

    /// 从无损 raw 结果构造（MCP adapter 落点）：`is_error` 取 raw 的 `isError`。
    pub fn from_raw_result(text: impl Into<String>, raw: RawToolResult) -> Self {
        Self {
            text: text.into(),
            execution: None,
            outcome: ToolOutcome::from_raw(raw),
        }
    }

    /// 携带显式 typed outcome 构造（可与执行证据组合）。
    pub fn with_outcome(text: impl Into<String>, outcome: ToolOutcome) -> Self {
        Self {
            text: text.into(),
            execution: None,
            outcome,
        }
    }

    /// 工具结果事实；[`ToolOutcome::Unreported`] = legacy 契约未报告。
    pub fn outcome(&self) -> &ToolOutcome {
        &self.outcome
    }

    /// 真实 `isError`：`Some(true/false)` = 工具如实报告；`None` = 未报告，不得推断。
    pub fn is_error(&self) -> Option<bool> {
        self.outcome.is_error()
    }

    /// 无损 raw 结果（若工具提供）。
    pub fn raw(&self) -> Option<&RawToolResult> {
        self.outcome.raw()
    }

    /// Produce the bounded model/live-event projection while retaining a
    /// compact execution summary whenever typed evidence exists.
    pub fn projected_text(&self, limit: Option<usize>) -> String {
        let summary = self
            .execution
            .as_ref()
            .map(ToolExecutionEvidence::render_summary)
            .unwrap_or_default();
        // Canonical messages may already contain this exact metadata-derived
        // suffix. Replay should reuse the projection without appending it a
        // second time; arbitrary body text is never parsed for status.
        if limit.is_none()
            && self
                .execution
                .as_ref()
                .is_some_and(|_| self.has_rendered_summary(&summary))
        {
            return self.text.clone();
        }
        if summary.is_empty() {
            return match limit {
                Some(limit) if self.text.chars().count() > limit => self.bounded_text(limit),
                _ => self.text.clone(),
            };
        }
        if let Some(limit) = limit {
            if self.text.chars().count() + 1 + summary.chars().count() <= limit {
                return format!("{}\n{}", self.text, summary);
            }
            let marker = format!("\n\n[Output truncated at {limit} chars]");
            let suffix = format!("{summary}{marker}");
            if suffix.chars().count() >= limit {
                if summary.chars().count() <= limit {
                    // The truncation marker is omitted, but the typed
                    // output_truncated fact still records why the body was
                    // shortened. Never cut a status/ref halfway through.
                    return summary;
                }
                return self
                    .execution
                    .as_ref()
                    .expect("summary is non-empty only with execution evidence")
                    .render_compact_summary(limit);
            }
            let head_budget = limit - suffix.chars().count();
            let head: String = self.text.chars().take(head_budget).collect();
            return format!("{head}{suffix}");
        }
        format!("{}\n{}", self.text, summary)
    }

    fn has_rendered_summary(&self, summary: &str) -> bool {
        if summary.is_empty() {
            return false;
        }
        let summary_suffix = format!("\n{summary}");
        self.text.ends_with(&summary_suffix)
            || self
                .text
                .split_once("\n\n[Output truncated at ")
                .is_some_and(|(body, _)| body.ends_with(&summary_suffix))
            || self.text == summary
    }

    pub fn bounded_text(&self, limit: usize) -> String {
        if self.text.chars().count() <= limit && self.execution.is_none() {
            return self.text.clone();
        }
        if self.execution.is_some() {
            return self.projected_text(Some(limit));
        }
        if self.text.chars().count() <= limit {
            return self.text.clone();
        }
        let marker = format!("\n\n[Output truncated at {limit} chars]");
        if marker.chars().count() >= limit {
            return marker.chars().take(limit).collect();
        }
        let head_budget = limit - marker.chars().count();
        let head: String = self.text.chars().take(head_budget).collect();
        format!("{head}{marker}")
    }

    pub fn body_was_truncated(&self, limit: Option<usize>) -> bool {
        let Some(limit) = limit else {
            return false;
        };
        let summary_len = self
            .execution
            .as_ref()
            .map(|evidence| 1 + evidence.render_summary().chars().count())
            .unwrap_or_default();
        self.text.chars().count() + summary_len > limit
    }
}

/// Todo 条目状态（L5：自 `peri-middlewares/src/tools/todo.rs` 迁入，
/// middlewares 保留 re-export；与 `crate::event::TodoStatus`（事件 DTO）同构
/// 但独立定义，避免改动事件序列化语义）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// Todo 列表条目（L5：自 `peri-middlewares/src/tools/todo.rs` 迁入契约层，
/// TodoWrite 工具 / 装配上下文 todo 通道共用；middlewares 保留 re-export）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    #[serde(
        default,
        rename = "activeForm",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_form: Option<String>,
    pub status: TodoStatus,
}

/// 工具定义（JSON Schema 格式参数描述）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: serde_json::Value,
}

/// 工具描述契约（design v2 §2.5.1，v1.4 新增）
///
/// 提示词层声明与 UI 展示使用的结构化描述。线上 LLM 投影仍为
/// [`ToolDefinition`]（name/description/parameters）——`title`/`namespace`
/// 仅存在于进程内契约与提示词层，不下发 API（OpenAI/Anthropic function
/// calling 无对应字段）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDescription {
    /// 模型调用名（必填，与 `BaseTool::name()` 一致）
    pub name: String,
    /// 模型向完整描述（必填，进入 API tools 列表）
    pub description: String,
    /// 短显示名（提示词层声明与 UI 展示引用；缺省由 [`derive_title_from_name`] 推导）
    pub title: Option<String>,
    /// 分组（提示词层按组组织声明段；缺省不分组）
    pub namespace: Option<String>,
}

/// 从工具名推导短显示名：CamelCase / snake_case 拆词、词首大写。
///
/// - `AskUserQuestion` → `Ask User Question`
/// - `folder_operations` → `Folder Operations`
/// - `Read` → `Read`
///
/// 仅处理 ASCII 字母数字与 `_`；连续大写/数字等超范围输入按最小规则尽力拆分
/// （design v2 §2.5.1「缺省时由 name 推导」）。
pub fn derive_title_from_name(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    // 前一个字符是否小写——camelCase 边界（小写 → 大写）据此切词
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '_' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            prev_lower = false;
        } else if c.is_ascii_uppercase() && prev_lower {
            words.push(std::mem::take(&mut current));
            current.push(c);
            prev_lower = false;
        } else {
            current.push(c);
            prev_lower = c.is_ascii_lowercase();
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
        .into_iter()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let mut capitalized = String::with_capacity(word.len());
                    capitalized.push(first.to_ascii_uppercase());
                    capitalized.push_str(chars.as_str());
                    capitalized
                }
                None => word,
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 工具上下文保留策略（用于 Compact 决策；自 peri-agent 迁入，
/// `peri-agent::tools::ContextRetention` 保留 re-export）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextRetention {
    /// 必须完整保留（用户回答、目标、任务状态工具）
    Preserve,
    /// 后续控制流依赖的状态（后续可能降级但不是现在）
    StateBearing,
    /// 副作用已完成的收据（只需保留摘要/状态）
    SideEffectReceipt,
    /// 可从磁盘/网络重新获取
    Recomputable,
}

// canonical 执行身份与来源的权威定义在 `tool_invocation`（见其模块文档）；
// 此处 re-export，消费方沿用 `tools::InvocationId` 等路径。
pub use crate::tool_invocation::{EffectiveToolCall, InvocationId, InvocationOrigin, ToolCallId};

/// 当前 turn 可供程序化调用的工具声明。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EffectiveToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 程序化 effective-tool 调用的稳定错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EffectiveToolErrorCode {
    UnknownTool,
    InvalidInput,
    PermissionDenied,
    UserRejected,
    Cancelled,
    Timeout,
    ApplicationFailed,
    ToolFailed,
}

impl EffectiveToolErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownTool => "UNKNOWN_TOOL",
            Self::InvalidInput => "INVALID_INPUT",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::UserRejected => "USER_REJECTED",
            Self::Cancelled => "CANCELLED",
            Self::Timeout => "TIMEOUT",
            Self::ApplicationFailed => "APPLICATION_FAILED",
            Self::ToolFailed => "TOOL_FAILED",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct EffectiveToolError {
    pub code: EffectiveToolErrorCode,
    pub message: String,
    /// Typed child failure facts, when this effective call crossed a subagent
    /// boundary.  The textual message remains the user-facing projection.
    pub subagent_failure: Option<Box<crate::error::SafeSubagentFailure>>,
    /// 远端 MCP 的 raw 错误事实（JSON-RPC `error` / 带内 `isError` 载荷）。
    /// `None` = 失败不来自远端 MCP（本地工具、权限、取消、超时等）。relay 与
    /// ACP 据此转发原始远端错误，不再靠 [`EffectiveToolErrorCode`] 反推。
    pub remote_error: Option<RemoteToolError>,
    /// 「审批不可用（无人可答）」的 typed 原因（裁决 R9）：只在本次失败是
    /// [`crate::error::ToolRejectionCause::ApprovalUnavailable`] 时存在。
    ///
    /// 与 [`EffectiveToolErrorCode::UserRejected`] 严格区分：userRejected 是
    /// **已作出的决定**，本字段是**没有人可以决定**。消费方（session outcome /
    /// relay）据此如实记录 `ApprovalUnavailable`，不靠带外记录点补事实。
    pub unanswered_cause: Option<UnansweredCause>,
}

impl EffectiveToolError {
    pub fn new(code: EffectiveToolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            subagent_failure: None,
            remote_error: None,
            unanswered_cause: None,
        }
    }

    pub fn with_subagent_failure(mut self, failure: crate::error::SafeSubagentFailure) -> Self {
        self.subagent_failure = Some(Box::new(failure));
        self
    }

    pub fn subagent_failure(&self) -> Option<&crate::error::SafeSubagentFailure> {
        self.subagent_failure.as_deref()
    }

    pub fn with_remote_error(mut self, remote: RemoteToolError) -> Self {
        self.remote_error = Some(remote);
        self
    }

    pub fn remote_error(&self) -> Option<&RemoteToolError> {
        self.remote_error.as_ref()
    }

    /// 附加「审批不可用（无人可答）」的 typed 原因。
    pub fn with_unanswered_cause(mut self, cause: UnansweredCause) -> Self {
        self.unanswered_cause = Some(cause);
        self
    }

    /// 「审批不可用（无人可答）」的 typed 原因；`None` = 本次失败与无人可答无关。
    pub fn unanswered_cause(&self) -> Option<UnansweredCause> {
        self.unanswered_cause
    }
}

/// 宿主工具进入 Agent canonical dispatch 的显式端口。
#[async_trait]
pub trait EffectiveToolDispatcher: Send + Sync {
    /// 执行一次 canonical effective call 并返回 typed outcome。
    ///
    /// 唯一入口（不再区分 `dispatch` / `dispatch_output`）：模型/UI 文本投影在
    /// [`ToolOutput::text`]，真实 `isError` 与无损 raw 结果在 [`ToolOutput::outcome`]，
    /// 本进程执行事实在 [`ToolOutput::execution`]。协议层失败（含远端 raw 错误）以
    /// [`EffectiveToolError`] 返回。
    async fn dispatch(
        &self,
        call: EffectiveToolCall,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ToolOutput, EffectiveToolError>;

    fn tools(&self) -> Vec<EffectiveToolDefinition>;

    /// Resolve an admitted MCP target by its source and original wire name.
    /// A colliding tool from another source must never authorize an App call.
    fn admitted_mcp_tool_name(&self, _server: &str, _wire_name: &str) -> Option<String> {
        None
    }
}

/// 当前会话上下文用量快照（工具只读运行时事实）。
///
/// 由 Agent 阶段在 dispatch 时投影：token 数为会话估算值，
/// 与状态栏 / 自动 Compact 使用同一口径（见 `TokenTracker::estimated_context_tokens`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    /// 估算的当前上下文 token 数。
    pub used_tokens: u64,
    /// 模型上下文窗口大小（token）。
    pub context_window: u32,
}

impl ContextUsage {
    /// 上下文使用率百分比（0-100）；窗口为 0 时无定义，返回 `None`。
    pub fn percent(&self) -> Option<f64> {
        (self.context_window > 0)
            .then(|| self.used_tokens as f64 / self.context_window as f64 * 100.0)
    }
}

/// 工具只读上下文（借用 state，零 clone）
///
/// 通过 `BaseTool::invoke` 的第二个参数传入。工具可读取 messages 和 cwd，
/// 但不能修改 state（避免绕过 dispatch_tools 统一写入语义）。
pub struct ToolContext<'a> {
    /// 当前对话历史（只读引用，借用 state.messages）
    pub messages: &'a [BaseMessage],
    /// 当前工作目录
    pub cwd: &'a str,
    /// 当前 canonical dispatch 能力；仅 dispatch 中调用工具时存在。
    pub effective_tool_dispatcher: Option<std::sync::Arc<dyn EffectiveToolDispatcher>>,
    /// 当前 canonical 执行身份；`None` = 不在 canonical dispatch 中。
    pub invocation_id: Option<InvocationId>,
    /// 当前执行的来源（模型 / 嵌套 / App / Host）；与 [`Self::invocation_id`] 同生共死。
    pub origin: Option<InvocationOrigin>,
    /// 模型 tool-call ID（与原始模型消息/卡片配对）；非模型发起时为 `None`。
    /// 与 [`Self::invocation_id`] 语义独立，不得互相替代。
    pub tool_call_id: Option<ToolCallId>,
    /// 当前外层调用的取消令牌。
    pub cancellation: tokio_util::sync::CancellationToken,
    /// 当前 Agent session identity；仅 canonical dispatch 中存在。
    pub session_id: Option<String>,
    /// 本次 canonical 执行的**交互归属事实**（谁在问）；`None` = 本次执行没有
    /// 可信归属（不在 canonical dispatch 中，或投影缺失）。
    ///
    /// 交互归属（工具发起审批 / 提问时）只能从这里取：工具不得自行生成归属、
    /// 不得从字符串或 ID 推断，缺失即显式拒绝（fail-closed）。
    pub attribution: Option<crate::interaction::InteractionAttribution>,
    /// Trusted terminal-reminder route into this (initiating) session's
    /// canonical transcript. `None` = no current delivery route available.
    pub task_terminal_delivery: Option<std::sync::Arc<dyn crate::tasks::TaskTerminalDelivery>>,
    /// 当前 turn generation；用于撤销跨 turn 的宿主调用租约。
    pub turn_generation: Option<String>,
    pub session_resources: Option<std::sync::Arc<dyn crate::session_resources::SessionResources>>,
    /// 当前会话上下文用量快照；`None` = 无预算或无估算，工具无法评估。
    pub context_usage: Option<ContextUsage>,
}

impl<'a> ToolContext<'a> {
    pub fn new(messages: &'a [BaseMessage], cwd: &'a str) -> Self {
        Self {
            messages,
            cwd,
            effective_tool_dispatcher: None,
            invocation_id: None,
            origin: None,
            tool_call_id: None,
            cancellation: tokio_util::sync::CancellationToken::new(),
            session_id: None,
            attribution: None,
            task_terminal_delivery: None,
            turn_generation: None,
            session_resources: None,
            context_usage: None,
        }
    }

    /// 绑定 dispatch 时刻的会话上下文用量快照。
    pub fn with_context_usage(mut self, usage: Option<ContextUsage>) -> Self {
        self.context_usage = usage;
        self
    }

    /// 绑定一次 canonical dispatch 的执行身份、来源与取消令牌。
    ///
    /// 执行身份只与 canonical dispatch 能力同时存在：能发起 canonical dispatch
    /// 的上下文才有 invocation 身份与来源。
    pub fn with_invocation(
        mut self,
        dispatcher: std::sync::Arc<dyn EffectiveToolDispatcher>,
        invocation_id: InvocationId,
        origin: InvocationOrigin,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Self {
        self.effective_tool_dispatcher = Some(dispatcher);
        self.invocation_id = Some(invocation_id);
        self.origin = Some(origin);
        self.cancellation = cancellation;
        self
    }

    /// 只写模型 tool-call ID：与原始模型消息/卡片配对，不涉及执行身份。
    pub fn with_tool_call_id(mut self, tool_call_id: impl Into<ToolCallId>) -> Self {
        self.tool_call_id = Some(tool_call_id.into());
        self
    }

    pub fn with_session_identity(
        mut self,
        session_id: impl Into<String>,
        turn_generation: impl Into<String>,
    ) -> Self {
        self.session_id = Some(session_id.into());
        self.turn_generation = Some(turn_generation.into());
        self
    }

    /// 绑定本次 canonical 执行的交互归属事实。
    ///
    /// 只有拿得到真实事实的调用方才绑定：canonical dispatch 从 `TurnContext`
    /// 投影模型归属，idle 入口（App / Host）从 typed 来源派生。缺失时交互必须被
    /// 显式拒绝，不得伪造归属。
    pub fn with_attribution(
        mut self,
        attribution: crate::interaction::InteractionAttribution,
    ) -> Self {
        self.attribution = Some(attribution);
        self
    }

    /// Attach the trusted terminal-reminder route of the executing session.
    pub fn with_task_terminal_delivery(
        mut self,
        delivery: std::sync::Arc<dyn crate::tasks::TaskTerminalDelivery>,
    ) -> Self {
        self.task_terminal_delivery = Some(delivery);
        self
    }
}

/// A target-specific canonical action bound before middleware/HITL.
///
/// `policy_name` and `policy_input` are the approval-safe projection. `target`
/// owns the immutable execution action and must not reinterpret the raw call.
#[derive(Clone)]
pub struct BoundToolInvocation {
    pub policy_name: String,
    pub policy_input: serde_json::Value,
    pub target: std::sync::Arc<dyn BaseTool>,
}

/// BaseTool trait - 对齐 LangChain Python BaseTool
///
/// 所有工具必须实现此 trait，不再依赖 langchain-rust::tools::Tool。
#[async_trait]
pub trait BaseTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    /// 返回完整工具定义（默认实现，组合 name/description/parameters）
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
    }

    /// 执行工具，输入为 JSON Value。
    ///
    /// 这是本地/legacy 工具的落点：只返回文本投影。**不是** MCP adapter 的落点——
    /// MCP 工具必须实现 [`BaseTool::invoke_output`]，否则 canonical dispatch 与
    /// ACP 只能拿到文本投影，raw 结果与真实 `isError` 在此丢失。
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;

    /// 执行工具并返回 typed outcome（无损 raw 结果 + 真实 `isError`）。
    ///
    /// 默认实现委托 [`BaseTool::invoke`]，语义明确为「raw 缺失」：
    /// [`ToolOutput::outcome`] 为 [`ToolOutcome::Unreported`]，
    /// [`ToolOutput::is_error`] 返回 `None`，调用方不得推断成功或失败。只有确实
    /// 持有 raw 结果的工具（MCP adapter、远端工具）才应覆盖本方法。
    async fn invoke_output(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
        self.invoke(input, ctx).await.map(ToolOutput::from_legacy)
    }

    /// Canonicalize one target invocation before middleware/HITL.
    ///
    /// Most tools return `None` and use the direct call. Method-based tools may
    /// return a bound execution target so approval never depends on a mutable
    /// raw input or on post-approval reparsing.
    fn bind_invocation(
        &self,
        _input: serde_json::Value,
    ) -> Result<Option<BoundToolInvocation>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(None)
    }

    /// Static MCP source identity used by session catalog shadowing and collision
    /// checks. Non-MCP tools return `None`; wrappers must forward this value.
    fn mcp_server_name(&self) -> Option<&str> {
        None
    }

    /// Original MCP wire name, independent of model-facing naming.
    fn mcp_tool_name(&self) -> Option<&str> {
        None
    }

    /// Trusted builtin transport identity. Never inferred from a tool/server name.
    fn builtin_mcp_instance(&self) -> Option<&str> {
        None
    }

    /// 工具调用的外层超时时间。None 表示不设外层超时（工具自行管理超时）。
    /// 默认 120s，适用于 Read/Edit/Glob 等快速操作。Agent/Bash 等工具应返回
    /// None，因为它们内部已有超时机制或需要长时间运行。
    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(120))
    }

    /// 工具声明的别名列表。当 LLM 输出的工具名匹配这些别名（大小写无关）时，
    /// 由 resolve_tool() 解析到本工具。典型用例：BashTool → aliases=["Shell"]。
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// 工具输出的默认截断长度（字符数）。None 表示不截断。
    fn output_char_limit(&self) -> Option<usize> {
        None
    }

    /// 工具输出是否偏向落盘而非内联返回。
    fn prefers_persist(&self) -> bool {
        false
    }

    /// 工具在上下文压缩中的保留策略。
    ///
    /// 默认返回 `ContextRetention::Preserve`——未显式标注的工具绝对不会被压缩。
    fn context_retention(&self) -> ContextRetention {
        ContextRetention::Preserve
    }

    /// 是否直接出现在 LLM 的 tools 参数中（无需经过 SearchExtraTools 发现）。
    /// 默认 `false`（安全默认值：新工具默认为 deferred）。
    fn is_direct(&self) -> bool {
        false
    }

    /// Whether this tool may be projected into the model-facing tool catalog.
    /// Host-only tools remain dispatchable through the canonical catalog/HITL.
    fn visible_to_model(&self) -> bool {
        true
    }

    /// 短显示名（≤ 6 词，名词短语）。缺省时由 [`derive_title_from_name`] 推导。
    fn title(&self) -> Option<&str> {
        None
    }

    /// 工具分组（如 `filesystem`、`web`、`meta`）。缺省不分组。
    fn namespace(&self) -> Option<&str> {
        None
    }

    /// 结构化工具描述（design v2 §2.5.1）：组装 name/description/title/namespace，
    /// title 缺省时由 name 推导。
    fn tool_description(&self) -> ToolDescription {
        ToolDescription {
            name: self.name().to_string(),
            description: self.description().to_string(),
            title: Some(
                self.title()
                    .map(str::to_owned)
                    .unwrap_or_else(|| derive_title_from_name(self.name())),
            ),
            namespace: self.namespace().map(str::to_owned),
        }
    }

    /// 提示词层声明模板；返回 `None` 表示不出现在提示词声明段（默认）。
    fn prompt_declaration(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
#[path = "tools_test.rs"]
mod tests;
