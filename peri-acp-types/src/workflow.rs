//! Workflow 协议契约（引擎 ↔ agent 回调类型）。
//!
//! 自 `peri-workflow`（`protocol.rs` / `progress.rs` / `runner.rs`）迁入
//! （3.0 批 2 波 1：协议类型归契约层；peri-workflow 保留 re-export 保兼容）。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Node → Rust 请求（agent 回调）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunParams {
    pub run_id: String,
    pub agent_id: u64,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}

/// AgentRunResult（对齐引擎 AgentRunResult）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AgentRunResult {
    #[serde(rename = "ok")]
    Ok {
        output: Value, // string 或 object
        usage: Usage,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", rename = "toolCount")]
        tool_count: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none", rename = "tokenCount")]
        token_count: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "durationMs"
        )]
        duration_ms: Option<u64>,
    },
    #[serde(rename = "skipped")]
    Skipped,
    #[serde(rename = "dead")]
    Dead {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl AgentRunResult {
    /// 提取 agent 执行中的 tool call 次数（仅 Ok 变体有值）
    pub fn tool_count(&self) -> Option<u64> {
        match self {
            AgentRunResult::Ok { tool_count, .. } => *tool_count,
            _ => None,
        }
    }

    /// 提取 agent 执行中的 token 消耗（仅 Ok 变体有值）
    pub fn token_count(&self) -> Option<u64> {
        match self {
            AgentRunResult::Ok { token_count, .. } => *token_count,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
}

/// ProgressEvent（对齐引擎 ProgressEvent 8 种类型）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProgressEvent {
    #[serde(rename = "run_started", rename_all = "camelCase")]
    RunStarted {
        run_id: String,
        workflow_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
    },
    #[serde(rename = "phase_started", rename_all = "camelCase")]
    PhaseStarted { run_id: String, phase: String },
    #[serde(rename = "phase_done", rename_all = "camelCase")]
    PhaseDone { run_id: String, phase: String },
    #[serde(rename = "agent_started", rename_all = "camelCase")]
    AgentStarted {
        run_id: String,
        agent_id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    #[serde(rename = "agent_progress", rename_all = "camelCase")]
    AgentProgress {
        run_id: String,
        agent_id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        /// 有效/解析后的模型名（serde-optional：旧版事件无此字段 → None）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// 请求的模型档位（alias，如 sonnet/haiku；alias 解析成功才有值，
        /// 脚本传具体模型名时为 None）。TUI 面板优先显示档位而非解析后的模型名。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_tier: Option<String>,
        /// 进度事件可只更新模型；旧版引擎始终提供计数。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_count: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_count: Option<u64>,
    },
    #[serde(rename = "agent_done", rename_all = "camelCase")]
    AgentDone {
        run_id: String,
        agent_id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        result: AgentRunResult,
    },
    #[serde(rename = "log", rename_all = "camelCase")]
    Log { run_id: String, message: String },
    #[serde(rename = "run_done", rename_all = "camelCase")]
    RunDone {
        run_id: String,
        status: String, // "completed" | "failed" | "killed"
        #[serde(skip_serializing_if = "Option::is_none")]
        return_value: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl ProgressEvent {
    pub fn run_id(&self) -> &str {
        match self {
            Self::RunStarted { run_id, .. }
            | Self::PhaseStarted { run_id, .. }
            | Self::PhaseDone { run_id, .. }
            | Self::AgentStarted { run_id, .. }
            | Self::AgentProgress { run_id, .. }
            | Self::AgentDone { run_id, .. }
            | Self::Log { run_id, .. }
            | Self::RunDone { run_id, .. } => run_id,
        }
    }
}

/// Per-phase agent count and token summary for notification formatting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSummary {
    pub name: String,
    pub agent_count: usize,
    pub token_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

// ─── progress 投影契约（自 peri-workflow/src/progress.rs 迁入）────────────
//
// `RunProgress`（含 `IndexMap` 字段）保留在 peri-workflow（契约层不引入
// indexmap 依赖），端口经 `runs_snapshot` 返回 JSON 透传。

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowMeta {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub phases: Vec<MetaPhase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaPhase {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseProgress {
    pub title: String,
    pub status: PhaseStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Pending,
    Active,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProgress {
    pub agent_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// 有效/解析后的模型名（运行中由 AgentProgress.model 携带，完成时以
    /// AgentRunResult::Ok.model 为准；旧版快照无此字段 → None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 请求的模型档位（alias，如 sonnet/haiku；alias 解析成功才有值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_tier: Option<String>,
    pub status: AgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<AgentRunResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Pending,
    Running,
    Done,
    Dead,
    Skipped,
}

// ─── registry 契约（自 peri-workflow/src/registry.rs 迁入）────────────────

// ─── workflow 结果投影契约 ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Running,
    Completed,
    Failed,
    Killed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceStatus {
    Passed,
    Failed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PostProcessingStatus {
    NotRequired,
    Passed,
    Failed,
    Blocked,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Deliverable,
    Blocked,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowWriteIntent {
    ReadOnly,
    Write {
        repo_root: String,
        cwd: String,
        path_allowlist: Vec<String>,
        #[serde(default)]
        head_may_change: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit_required: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAttempt {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<u64>,
    pub journal_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_from: Option<RecoveredAttempt>,
    pub consumed: bool,
    pub disposition: AttemptDisposition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveredAttempt {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<u64>,
    pub journal_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptDisposition {
    Produced,
    Recovered,
    Rejected,
}

/// Workflow run 状态（registry 与 task result 共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

impl WorkflowRunStatus {
    /// 序列化为协议字符串（与 `ProgressEvent::RunDone.status` / state.json 口径一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkflowRunStatus::Running => "running",
            WorkflowRunStatus::Completed => "completed",
            WorkflowRunStatus::Failed => "failed",
            WorkflowRunStatus::Killed => "killed",
        }
    }
}

/// Workflow 完成后通过 broadcast channel 推送的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTaskResult {
    pub run_id: String,
    pub workflow_name: String,
    pub success: bool,
    pub status: WorkflowRunStatus,
    #[serde(default)]
    pub execution_status: ExecutionStatus,
    #[serde(default)]
    pub acceptance_status: AcceptanceStatus,
    #[serde(default)]
    pub post_processing_status: PostProcessingStatus,
    #[serde(default)]
    pub delivery_status: DeliveryStatus,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub state_artifact_exists: bool,
    pub duration_ms: u64,
    pub agent_count: usize,
    pub tool_calls_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_summaries: Vec<PhaseSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<WorkflowAttempt>,
}

fn escape_reminder_text(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl WorkflowTaskResult {
    /// 格式化为 `<system-reminder>` 块，含 phase breakdown。
    pub fn to_notification(&self) -> String {
        let success_msg = if self.success { "completed" } else { "failed" };

        let mut phase_lines = String::new();
        if !self.phase_summaries.is_empty() {
            for s in &self.phase_summaries {
                let token_info = if s.token_count > 0 {
                    format!(", {} tokens", s.token_count)
                } else {
                    String::new()
                };
                let dur_info = if let Some(d) = s.duration_ms {
                    format!(", {}ms", d)
                } else {
                    String::new()
                };
                phase_lines.push_str(&format!(
                    "- {}: {} agents{}{}\n",
                    escape_reminder_text(&s.name),
                    s.agent_count,
                    token_info,
                    dur_info
                ));
            }
        }

        let error_line = if self.success {
            String::new()
        } else {
            let error = self
                .error
                .as_deref()
                .filter(|error| !error.trim().is_empty())
                .unwrap_or("no error details available");
            let error = escape_reminder_text(&crate::session::sanitize_public_error(error, 2_000));
            format!("Error: {error}\n")
        };
        let state_path = format!(
            ".claude/workflow-runs/{}/state.json",
            escape_reminder_text(&self.run_id)
        );
        let artifact_lines = if self.state_artifact_exists {
            format!("Results saved to {state_path}\nUse Read tool to view full results.\n")
        } else {
            "Result state file was not generated.\n".to_string()
        };
        let projection_line = format!(
            "Execution: {:?}; acceptance: {:?}; post-processing: {:?}; delivery: {:?}.\n",
            self.execution_status,
            self.acceptance_status,
            self.post_processing_status,
            self.delivery_status,
        )
        .to_ascii_lowercase();

        let attempt_lines = self
            .attempts
            .iter()
            .map(|attempt| {
                format!(
                    "- attempt run_id={} agent_id={} journal_seq={} disposition={:?} consumed={}{}\n",
                    escape_reminder_text(&attempt.run_id),
                    attempt.agent_id.map_or("unknown".to_string(), |id| id.to_string()),
                    attempt.journal_seq,
                    attempt.disposition,
                    attempt.consumed,
                    attempt.recovered_from.as_ref().map_or_else(String::new, |source| {
                        format!(
                            " recovered_from={}/{}/{}",
                            escape_reminder_text(&source.run_id),
                            source
                                .agent_id
                                .map_or("unknown".to_string(), |id| id.to_string()),
                            source.journal_seq
                        )
                    })
                )
                .to_ascii_lowercase()
            })
            .collect::<String>();

        format!(
            "<system-reminder>\n\
            Workflow '{}' {}. ({}ms, {} agents, {} tool calls)\n\
            {}{}{}{}{}\
            </system-reminder>",
            escape_reminder_text(&self.workflow_name),
            success_msg,
            self.duration_ms,
            self.agent_count,
            self.tool_calls_count,
            phase_lines,
            error_line,
            projection_line,
            attempt_lines,
            artifact_lines,
        )
    }
}

/// Agent 回调执行器 trait（由 peri-acp 实现）
#[async_trait]
pub trait AgentExecutor: Send + Sync {
    /// 执行单个 workflow agent，返回 AgentRunResult
    async fn execute(&self, params: AgentRunParams) -> AgentRunResult;
}
