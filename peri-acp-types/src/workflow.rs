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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        /// Independent of the first failure: unsettled resources must fence the run.
        #[serde(
            default,
            skip_serializing_if = "std::ops::Not::not",
            rename = "scopeCloseIncomplete"
        )]
        scope_close_incomplete: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Measured execution time for this attempt; `None` means not measured.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "durationMs"
        )]
        duration_ms: Option<u64>,
        /// Output tokens observed before the attempt died; `None` means not reported.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "tokenCount"
        )]
        token_count: Option<u64>,
    },
}

impl AgentRunResult {
    /// A completed assignment with unconfirmed resources must fence its run before retry.
    pub fn unsettled_scope_error(&self) -> Option<&str> {
        match self {
            Self::Dead {
                reason,
                detail,
                scope_close_incomplete,
                ..
            } if *scope_close_incomplete || reason.as_deref() == Some("scope-close-incomplete") => {
                Some(detail.as_deref().unwrap_or(
                    "Workflow agent scope did not close; resource settlement is unconfirmed",
                ))
            }
            _ => None,
        }
    }

    /// Preserve the first failure while independently propagating scope uncertainty.
    pub fn after_scope_close(self, closed: bool) -> Self {
        if closed {
            return self;
        }
        match self {
            Self::Dead {
                reason,
                detail,
                duration_ms,
                token_count,
                ..
            } => Self::Dead {
                reason,
                detail,
                scope_close_incomplete: true,
                duration_ms,
                token_count,
            },
            _ => Self::Dead {
                reason: Some("scope-close-incomplete".into()),
                detail: Some("Workflow agent scope did not close; outstanding resources are unconfirmed. Do not start dependent writes or verification.".into()),
                scope_close_incomplete: true,
                duration_ms: None,
                token_count: None,
            },
        }
    }

    /// 提取 agent 执行中的 tool call 次数（仅 Ok 变体有值）
    pub fn tool_count(&self) -> Option<u64> {
        match self {
            AgentRunResult::Ok { tool_count, .. } => *tool_count,
            _ => None,
        }
    }

    /// 提取 agent 执行中的 token 消耗（Ok 与带指标的 Dead 变体）
    pub fn token_count(&self) -> Option<u64> {
        match self {
            AgentRunResult::Ok { token_count, .. } | AgentRunResult::Dead { token_count, .. } => {
                *token_count
            }
            _ => None,
        }
    }

    /// 迭代上限耗尽：执行器用稳定 reason 编码上报，调用方不解析错误文案。
    #[must_use]
    pub fn iteration_limit_exhausted(&self) -> bool {
        matches!(
            self,
            AgentRunResult::Dead {
                reason: Some(reason),
                ..
            } if reason == AGENT_DEAD_REASON_MAX_ITERATIONS
        )
    }
}

/// 迭代上限耗尽的 dead reason（`AgentError::MaxIterationsExceeded` 的结构化投影）。
pub const AGENT_DEAD_REASON_MAX_ITERATIONS: &str = "max-iterations";

/// 单次 workflow agent 执行实际解析到的硬上限与用量。
///
/// 由执行器（peri-agent）在解析后按 `(run_id, agent_id)` 记录，宿主在结算时读取；
/// 只描述本次执行，不改变 resume 重放与限额策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExecutionLimits {
    /// 实际解析后的迭代上限（不写死默认值：无 agentType 的回落值与定义 `max_turns` 各按解析结果）。
    pub iteration_limit: u64,
    pub iteration_limit_source: IterationLimitSource,
    /// 已用语义迭代数；执行未进入循环或未采集时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations_used: Option<u64>,
}

/// 有效迭代上限的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IterationLimitSource {
    /// 未指定 agentType（或定义未给出正向 max_turns）：执行器回落的上限。
    Default,
    /// agent 定义的 `max_turns`（含 builtin coder 的默认值）。
    AgentDefinition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    #[must_use]
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
    /// 相位**墙钟**毫秒（宿主收到 `PhaseStarted` → 收到 `PhaseDone` 的时长差）：
    /// 并行相位因此重合而非相加。无相位边界事件时为 `None`（resume 命中、引擎
    /// 未发相位事件、无 `phase()` 的 agent 归组）；生产者不得回落成相位内 agent
    /// 时长求和——那是另一维度的量，曾把并行相位读成「重叠 / 超长」
    /// （2026-10-11 issue 缺陷 3）。
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
    Blocked,
    /// 历史 `state.json` 可能写着 `deliverable`（该投影分支已删，内核不再产出）：
    /// 继续按 `unknown` 读入，是**数据兼容**而非 API 兼容——枚举收窄不得让
    /// resume / 审计读旧文件失败。
    #[serde(alias = "deliverable")]
    #[default]
    Unknown,
}

/// 交付结论的结构化原因。
///
/// `delivery_status` 只有 `blocked` / `unknown`，无法区分“未请求交付”
/// “无法归因的变化”和“观测不完整”。旧消费者忽略本字段；缺失不是“已通过”。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryReason {
    /// 未声明 writeIntent：本次运行没有请求交付，不能读作写入验收通过。
    NotRequested,
    /// 观察到差异但无法归因给本 run。
    UnattributedChange,
    /// 无基线、采样失败或范围不可验证：不得据此宣称“无变化”。
    ObservationIncomplete,
}

/// Git 观测窗口的完整性（观测事实，不是交付结论）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostconditionObservation {
    /// 基线已采样，运行前后可以对比。
    Complete,
    /// 非 Git 仓库：没有可绑定的仓库范围。
    NotApplicable,
    /// 采样失败或验证范围不可用。
    Incomplete,
}

/// 运行所在的 Git 身份与基线可见范围（观测事实，不是交付结论）。
///
/// 终态只持久化身份与能力，不保存基线内容（porcelain 字节）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitObservationScope {
    pub repo_root: String,
    pub cwd: String,
    /// `git rev-parse --git-dir` 解析后的绝对路径。
    pub git_dir: String,
    /// `git rev-parse --git-common-dir` 解析后的绝对路径。
    pub git_common_dir: String,
    /// 采样时的 HEAD。
    pub head: String,
    /// 基线实际覆盖的范围；未列出的都不在观测范围内。
    pub visibility: GitObservationVisibility,
}

/// 基线能观测到什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitObservationVisibility {
    /// `git status --porcelain=v2` 观测 index。
    pub index: bool,
    /// 观测 worktree（含未跟踪文件）。
    pub worktree: bool,
    /// ignored 文件不进入基线，其变化不可观测。
    pub ignored: bool,
    /// 只比较运行前后的 HEAD 端点，不覆盖完整提交区间与提交内容。
    pub commit_range: bool,
}

/// 引擎级结论：只聚合 execution 与 post-processing/delivery，不代替语义验收。
///
/// `Completed` 仅表示执行完成且 post-condition 未阻断；`acceptance_status` 仍是
/// 独立字段，内核恒为 `unknown`（ADLC-DELIVERY-STATUS-001）。因此报告不得把
/// `Completed` 读成“任务成功”，三态必须与 acceptance 分开呈现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOutcome {
    Completed,
    Blocked,
    Failed,
    Killed,
    #[default]
    Unknown,
}

impl WorkflowOutcome {
    /// 单一派生点：execution 终态优先，其次 delivery/acceptance 阻断。
    #[must_use]
    pub fn project(
        execution: ExecutionStatus,
        acceptance: AcceptanceStatus,
        delivery: DeliveryStatus,
    ) -> Self {
        match execution {
            ExecutionStatus::Failed => Self::Failed,
            ExecutionStatus::Killed => Self::Killed,
            ExecutionStatus::Running | ExecutionStatus::Unknown => Self::Unknown,
            ExecutionStatus::Completed => {
                if acceptance == AcceptanceStatus::Failed || delivery == DeliveryStatus::Blocked {
                    Self::Blocked
                } else {
                    Self::Completed
                }
            }
        }
    }
}

/// 旧 payload 没有 `outcome` 时的派生：沿用当时的成功口径
/// （`status == completed && delivery != blocked`），acceptance 不参与，
/// 避免回填一个旧数据从未携带的裁决。
fn legacy_outcome(status: WorkflowRunStatus, delivery: DeliveryStatus) -> WorkflowOutcome {
    let execution = match status {
        WorkflowRunStatus::Running => ExecutionStatus::Running,
        WorkflowRunStatus::Completed => ExecutionStatus::Completed,
        WorkflowRunStatus::Failed => ExecutionStatus::Failed,
        WorkflowRunStatus::Killed => ExecutionStatus::Killed,
    };
    WorkflowOutcome::project(execution, AcceptanceStatus::Unknown, delivery)
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
    /// 1-based execution attempt ordinal for this journal sequence. Host-enriched;
    /// `None` for legacy entries and cache hits whose original ordinal is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_from: Option<RecoveredAttempt>,
    pub consumed: bool,
    pub disposition: AttemptDisposition,
    /// 本次尝试的执行结果（宿主在写入 journal 时补注）。
    ///
    /// 与 `disposition` 正交：`disposition=produced` 只表示结果来源，
    /// 执行失败/超限仍如实出现在这里；旧 journal 缺省为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<AttemptExecution>,
}

/// attempt 的实际执行结果（不是交付结论）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptExecution {
    pub outcome: AttemptOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 执行器实际解析后的迭代上限；未解析（如执行前失败）为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_limit_source: Option<IterationLimitSource>,
    /// 已用语义迭代数；未采集为 `None`（不得写成 0）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations_used: Option<u64>,
    /// run 的 `budgetTotal`；未配置为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    /// 本次执行上报的 token 用量；未采集为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
}

/// 执行的结局类别：与 `AgentRunResult` 变体对齐，`Limited` 表示迭代上限耗尽。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Ok,
    Dead,
    Skipped,
    Limited,
}

/// 已触发硬上限的范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitScope {
    ToolCalls,
    Tokens,
    Iterations,
    ElapsedMs,
    Agents,
}

/// 一次已触发的硬上限（`state.json` 的 `limit_breaches`）。
///
/// 与顶层 `error` 并存：`error` 仍是首条 breach 的兼容文本，这里列全部触发项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitBreach {
    pub scope: LimitScope,
    /// 配置的上限；未采集到具体数值时为 `None`（不得写成 0）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// 观测用量；未采集为 `None`（不得写成 0）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<u64>,
    /// 触发它的 attempt 身份；run 级超限（如 `maxElapsedMs`）没有单次 attempt。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<LimitBreachAttempt>,
    /// 检测时点（宿主时钟，RFC 3339）。
    pub detected_at: String,
}

/// 超限归因的尝试身份（taskId 作用域 = 单次 run）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitBreachAttempt {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub agent_id: u64,
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
#[serde(from = "WorkflowTaskResultWire")]
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
    /// 引擎级三态（`completed` / `blocked` / `unknown`）；acceptance 仍是独立事实。
    /// 旧 payload 缺字段时由 `status + delivery_status` 派生，显式 `unknown` 保持
    /// `unknown`（反序列化经 `WorkflowTaskResultWire` 区分两种输入）。
    pub outcome: WorkflowOutcome,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub state_artifact_exists: bool,
    pub duration_ms: u64,
    pub agent_count: usize,
    pub tool_calls_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Bounded script output for the completion consumer, not an engine verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<WorkflowResultPreview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_summaries: Vec<PhaseSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<WorkflowAttempt>,
}

/// `WorkflowTaskResult` 的反序列化影子：`outcome` 用 `Option` 保留「字段缺失」
/// 与「显式 `unknown`」的区别，其余字段保持 wire 形状；新增 wire 字段需与公开
/// 结构同步。
#[derive(Deserialize)]
struct WorkflowTaskResultWire {
    run_id: String,
    workflow_name: String,
    success: bool,
    status: WorkflowRunStatus,
    #[serde(default)]
    execution_status: ExecutionStatus,
    #[serde(default)]
    acceptance_status: AcceptanceStatus,
    #[serde(default)]
    post_processing_status: PostProcessingStatus,
    #[serde(default)]
    delivery_status: DeliveryStatus,
    #[serde(default)]
    outcome: Option<WorkflowOutcome>,
    #[serde(default)]
    state_artifact_exists: bool,
    duration_ms: u64,
    agent_count: usize,
    tool_calls_count: usize,
    error: Option<String>,
    #[serde(default)]
    result_preview: Option<WorkflowResultPreview>,
    #[serde(default)]
    phase_summaries: Vec<PhaseSummary>,
    #[serde(default)]
    attempts: Vec<WorkflowAttempt>,
}

impl From<WorkflowTaskResultWire> for WorkflowTaskResult {
    fn from(wire: WorkflowTaskResultWire) -> Self {
        let outcome = wire
            .outcome
            .unwrap_or_else(|| legacy_outcome(wire.status, wire.delivery_status));
        Self {
            run_id: wire.run_id,
            workflow_name: wire.workflow_name,
            success: wire.success,
            status: wire.status,
            execution_status: wire.execution_status,
            acceptance_status: wire.acceptance_status,
            post_processing_status: wire.post_processing_status,
            delivery_status: wire.delivery_status,
            outcome,
            state_artifact_exists: wire.state_artifact_exists,
            duration_ms: wire.duration_ms,
            agent_count: wire.agent_count,
            tool_calls_count: wire.tool_calls_count,
            error: wire.error,
            result_preview: wire.result_preview,
            phase_summaries: wire.phase_summaries,
            attempts: wire.attempts,
        }
    }
}

pub const WORKFLOW_RESULT_PREVIEW_BYTES: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowResultPreview {
    pub text: String,
    /// False when output was truncated or an external reference could not be read.
    #[serde(default)]
    pub complete: bool,
}

impl WorkflowResultPreview {
    pub fn bounded_text(&self) -> (&str, bool) {
        let mut end = self.text.len().min(WORKFLOW_RESULT_PREVIEW_BYTES);
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        (&self.text[..end], self.complete && end == self.text.len())
    }
}

fn escape_reminder_text(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 终态四维词表：给消费方（模型/客户端）可读表达，不用 `Debug` 派生的小写——
/// `Debug` 把 `NotRequired` 渲染成 `notrequired`，与 `unknown` 并列时被读成
/// 「未验证」（2026-10-11 issue 缺陷 2）。
fn execution_status_word(status: ExecutionStatus) -> &'static str {
    match status {
        ExecutionStatus::Running => "running",
        ExecutionStatus::Completed => "completed",
        ExecutionStatus::Failed => "failed",
        ExecutionStatus::Killed => "killed",
        ExecutionStatus::Unknown => "unknown",
    }
}

fn acceptance_status_word(status: AcceptanceStatus) -> &'static str {
    match status {
        AcceptanceStatus::Passed => "passed",
        AcceptanceStatus::Failed => "failed",
        // 内核不判验收（ADLC-DELIVERY-STATUS-001）：`unknown` 是常态，不是缺失。
        AcceptanceStatus::Unknown => "unknown (not judged by the engine)",
    }
}

fn post_condition_word(status: PostProcessingStatus) -> &'static str {
    match status {
        // read_only 通过 = 仓库前后未被改动，读作「已验证」而不是「未处理」。
        PostProcessingStatus::NotRequired => "passed (read-only verified)",
        PostProcessingStatus::Passed => "passed (write intent verified)",
        PostProcessingStatus::Failed => "failed",
        // 无基线、采样失败与无法归因都落在这里：只陈述“未验证”，不猜测原因。
        PostProcessingStatus::Blocked => "blocked (post-condition not verified)",
        PostProcessingStatus::Unknown => "unknown",
    }
}

fn delivery_status_word(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Blocked => "blocked",
        DeliveryStatus::Unknown => "unknown",
    }
}

fn outcome_word(outcome: WorkflowOutcome) -> &'static str {
    match outcome {
        WorkflowOutcome::Completed => "completed (engine-level; acceptance not judged)",
        WorkflowOutcome::Blocked => "blocked",
        WorkflowOutcome::Failed => "failed",
        WorkflowOutcome::Killed => "killed",
        WorkflowOutcome::Unknown => "unknown",
    }
}

/// `return_value` **对象字段**的长文本外置阈值（**字节**，非字符数）：超过它的
/// 字符串在写 `state.json` 前被抽出到 `outputs/<label>.txt`，原位替换为 `${label}`
/// 引用。
///
/// 单一权威：宿主提取的调用点（`peri-workflow` 的 `extract_long_texts` 调用）
/// 与提醒/启动文案（`WorkflowTaskResult::to_notification`、
/// `peri-workflow` 的启动文案）共用本常量。CJK 一字 3 字节，所以「200 字节」
/// 对中文内容远早于 200 字触发；改这里即同时改变提取行为与对外说明。
/// 口径见 `docs/design/workflow.md` §3.6。
///
/// 作用面：宿主提取点位于 `workflow/done` 处理，且只对**对象形态**的 `return`
/// 生效（`extract_long_texts` 本身支持数组/嵌套，但调用点的 gate 排除了顶层数组
/// 与顶层字符串）。文案必须与 gate 一致，不得声称「所有超长字符串都会外置」。
pub const LONGTEXT_EXTRACT_THRESHOLD_BYTES: usize = 200;

impl WorkflowTaskResult {
    /// Canonical background task projection, shared by owner and MCP consumer.
    pub fn background_result(&self) -> crate::event::BackgroundTaskResult {
        crate::event::BackgroundTaskResult {
            task_id: self.run_id.clone(),
            agent_name: format!("workflow:{}", self.workflow_name),
            prompt_summary: self.workflow_name.clone(),
            success: self.agent_facing_success(),
            output: self.to_notification(),
            tool_calls_count: self.tool_calls_count,
            duration_ms: self.duration_ms,
            child_thread_id: None,
            timed_out: false,
            subagent_failure: None,
            shell_output: None,
        }
    }

    /// 主 Agent 是否应把 workflow 视为「成功交付」（非仅 engine 跑完）。
    #[must_use]
    pub fn agent_facing_success(&self) -> bool {
        self.outcome == WorkflowOutcome::Completed
    }

    /// Defer / 通知用状态短语：`delivery_status=blocked` 时不得写 "completed"。
    pub fn notification_status_phrase(&self) -> &'static str {
        match self.status {
            WorkflowRunStatus::Running => "running",
            WorkflowRunStatus::Completed => {
                if self.delivery_status == DeliveryStatus::Blocked {
                    "finished (engine completed; delivery blocked)"
                } else {
                    "completed"
                }
            }
            WorkflowRunStatus::Killed => "killed",
            WorkflowRunStatus::Failed => "failed",
        }
    }

    /// 格式化 workflow 完成通知正文，含 phase breakdown。
    ///
    /// 本函数不编码 `<system-reminder>` envelope；canonical producer 在队列/投影边界
    /// 统一编码，避免 producer 直接拼接 wire format。
    pub fn to_notification(&self) -> String {
        let success_msg = self.notification_status_phrase();

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

        let error_line = if self.status == WorkflowRunStatus::Failed
            || self.delivery_status == DeliveryStatus::Blocked
            || self
                .error
                .as_deref()
                .is_some_and(|error| !error.trim().is_empty())
        {
            let error = self
                .error
                .as_deref()
                .filter(|error| !error.trim().is_empty())
                .unwrap_or(if self.delivery_status == DeliveryStatus::Blocked {
                    "delivery is blocked; inspect the post-condition and required evidence"
                } else {
                    "no error details available"
                });
            let error = escape_reminder_text(&crate::session::bounded_error_message(error, 2_000));
            format!("Error: {error}\n")
        } else {
            String::new()
        };
        let state_path = format!(
            ".claude/workflow-runs/{}/state.json",
            escape_reminder_text(&self.run_id)
        );
        let (result_lines, inline_complete) = match &self.result_preview {
            Some(preview) => {
                let (text, complete) = preview.bounded_text();
                let completeness = if complete {
                    "complete"
                } else {
                    "partial; not a complete result"
                };
                (format!(
                    "Reported result ({completeness}; workflow output, not an engine acceptance verdict):\n{}\n",
                    escape_reminder_text(text)
                ), complete)
            }
            None => (String::new(), false),
        };
        let artifact_lines = if self.state_artifact_exists && inline_complete {
            format!(
                "Full artifacts saved to {state_path}. Use the inline result when it contains the \
                 evidence needed for the handoff; do not restart agents just to repeat their report.\n"
            )
        } else if self.state_artifact_exists {
            // 指引指代 builtin 文件工具时必须用**模型面名字**（裸名已无提供面）；
            // 查表未命中的兜底不含工具名，不得回落到裸名。
            // `${label}` 是写入期契约（阈值见 LONGTEXT_EXTRACT_THRESHOLD_BYTES）：
            // 不说明它是引用，读 state.json 的消费方会把 token 当结果、静默丢长文本。
            let read_lead = crate::builtin_mcp::model_tool_name_of("workspace", "Read")
                .map_or("Read it".to_string(), |name| format!("Use `{name}`"));
            let threshold = LONGTEXT_EXTRACT_THRESHOLD_BYTES;
            format!(
                "Results saved to {state_path}\n\
                 Long string fields in the result object (over {threshold} bytes) are externalized: \
                 a `${{label}}` token in state.json is that field's path, and its text lives in \
                 `outputs/` under the same label with a `.txt` suffix.\n\
                 {read_lead} to view full results.\n"
            )
        } else {
            "Result state file was not generated.\n".to_string()
        };
        let projection_line = format!(
            "Execution: {}; outcome: {}; acceptance: {}; post-condition: {}; delivery: {}.\n\
             Acceptance is judged outside the engine, so it reads `unknown` unless the calling \
             layer (ADLC/skill) supplies a verdict; outcome `completed` is engine-level and does \
             not mean task success.\n",
            execution_status_word(self.execution_status),
            outcome_word(self.outcome),
            acceptance_status_word(self.acceptance_status),
            post_condition_word(self.post_processing_status),
            delivery_status_word(self.delivery_status),
        );

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
            "Workflow '{}' {}. ({}ms, {} agents, {} tool calls)\n\
            {}{}{}{}{}{}",
            escape_reminder_text(&self.workflow_name),
            success_msg,
            self.duration_ms,
            self.agent_count,
            self.tool_calls_count,
            phase_lines,
            error_line,
            projection_line,
            attempt_lines,
            result_lines,
            artifact_lines,
        )
    }
}

/// Agent 回调执行器 trait（由 peri-acp 实现）
#[async_trait]
pub trait AgentExecutor: Send + Sync {
    /// Bind the session task manager before any workflow agent is admitted.
    fn bind_execution_manager(
        &self,
        _manager: std::sync::Arc<dyn crate::tasks::TaskManager>,
    ) -> Result<(), String> {
        Ok(())
    }
    /// 执行单个 workflow agent，返回 AgentRunResult
    async fn execute(&self, params: AgentRunParams) -> AgentRunResult;

    /// 本次执行实际解析到的硬上限与用量（宿主结算路径读取）。
    ///
    /// 默认未知：限额数值只由执行器在解析后给出，宿主不得自行推断或写死默认值。
    fn execution_limits(&self, _run_id: &str, _agent_id: u64) -> Option<AgentExecutionLimits> {
        None
    }

    /// run 结束时回收该 run 的执行事实（无状态实现无需覆盖）。
    fn forget_run(&self, _run_id: &str) {}
}

#[cfg(test)]
#[path = "workflow_test.rs"]
mod tests;

#[cfg(test)]
mod scope_close_tests {
    use super::AgentRunResult;

    #[test]
    fn first_failure_and_scope_uncertainty_round_trip_independently() {
        for reason in [
            "event-forwarder-failed",
            "initial-prompt-enqueue-failed",
            "execution-blocked",
            "interrupted",
        ] {
            let original: AgentRunResult = serde_json::from_value(serde_json::json!({
                "kind": "dead", "reason": reason, "detail": "first cause"
            }))
            .unwrap();
            assert!(original.unsettled_scope_error().is_none());
            assert!(serde_json::to_value(&original)
                .unwrap()
                .get("scopeCloseIncomplete")
                .is_none());
            let closed = original.clone().after_scope_close(true);
            assert_eq!(closed, original);
            let unsettled = original.after_scope_close(false);
            assert!(unsettled.unsettled_scope_error().is_some());
            let wire = serde_json::to_value(&unsettled).unwrap();
            assert_eq!(wire["reason"], reason);
            assert_eq!(wire["detail"], "first cause");
            assert_eq!(wire["scopeCloseIncomplete"], true);
            let restored: AgentRunResult = serde_json::from_value(wire).unwrap();
            assert_eq!(restored, unsettled);
            assert!(restored.unsettled_scope_error().is_some());
        }
    }

    #[test]
    fn legacy_scope_failure_still_fences() {
        let legacy: AgentRunResult = serde_json::from_value(serde_json::json!({
            "kind": "dead", "reason": "scope-close-incomplete"
        }))
        .unwrap();
        assert!(legacy.unsettled_scope_error().is_some());
        assert!(AgentRunResult::Skipped
            .after_scope_close(false)
            .unsettled_scope_error()
            .is_some());
    }
}
