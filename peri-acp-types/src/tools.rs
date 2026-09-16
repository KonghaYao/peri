//! 工具契约类型（自 peri-agent 迁入，`peri-agent::tools` 保留 re-export）。

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::messages::BaseMessage;

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

/// Tool text plus optional execution evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ToolExecutionEvidence>,
}

impl ToolOutput {
    pub fn from_legacy(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            execution: None,
        }
    }

    pub fn with_execution(text: impl Into<String>, execution: ToolExecutionEvidence) -> Self {
        Self {
            text: text.into(),
            execution: Some(execution),
        }
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

/// Programmatic Tool Calling 的 public canonical 工具名。
///
/// 置于工具契约 crate，供 Agent dispatch guard 与 middleware 实现共享，避免
/// `peri-agent` 反向依赖 `peri-middlewares`。
pub const RUN_PTC_CODE_TOOL_NAME: &str = "RunPtcCode";

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

/// 可由宿主工具发起的 canonical effective-tool 调用。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveToolCall {
    pub invocation_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub parent_invocation_id: Option<String>,
}

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
    pub subagent_failure: Option<crate::error::SafeSubagentFailure>,
}

impl EffectiveToolError {
    pub fn new(code: EffectiveToolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            subagent_failure: None,
        }
    }

    pub fn with_subagent_failure(mut self, failure: crate::error::SafeSubagentFailure) -> Self {
        self.subagent_failure = Some(failure);
        self
    }

    pub fn subagent_failure(&self) -> Option<&crate::error::SafeSubagentFailure> {
        self.subagent_failure.as_ref()
    }
}

/// 宿主工具进入 Agent canonical dispatch 的显式端口。
#[async_trait]
pub trait EffectiveToolDispatcher: Send + Sync {
    async fn dispatch(
        &self,
        call: EffectiveToolCall,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String, EffectiveToolError>;

    /// Execute an effective call while retaining typed evidence when the
    /// target owns lifecycle facts. The compatibility default deliberately
    /// leaves legacy string-only dispatches unknown.
    async fn dispatch_output(
        &self,
        call: EffectiveToolCall,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ToolOutput, EffectiveToolError> {
        self.dispatch(call, cancel)
            .await
            .map(ToolOutput::from_legacy)
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition>;
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
    /// 当前外层 tool call ID，供宿主工具关联内部 invocation。
    pub invocation_id: Option<String>,
    /// 当前外层调用的取消令牌。
    pub cancellation: tokio_util::sync::CancellationToken,
    /// 当前 Agent session identity；仅 canonical dispatch 中存在。
    pub session_id: Option<String>,
    /// 当前 turn generation；用于撤销跨 turn 的宿主调用租约。
    pub turn_generation: Option<String>,
}

impl<'a> ToolContext<'a> {
    pub fn new(messages: &'a [BaseMessage], cwd: &'a str) -> Self {
        Self {
            messages,
            cwd,
            effective_tool_dispatcher: None,
            invocation_id: None,
            cancellation: tokio_util::sync::CancellationToken::new(),
            session_id: None,
            turn_generation: None,
        }
    }

    pub fn with_effective_tool_dispatcher(
        mut self,
        dispatcher: std::sync::Arc<dyn EffectiveToolDispatcher>,
        invocation_id: impl Into<String>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Self {
        self.effective_tool_dispatcher = Some(dispatcher);
        self.invocation_id = Some(invocation_id.into());
        self.cancellation = cancellation;
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

    /// 执行工具，输入为 JSON Value
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;

    /// Execute with optional typed evidence. The compatibility default keeps
    /// legacy tools' execution state unknown; only tools that own process or
    /// lifecycle facts should override this method.
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
