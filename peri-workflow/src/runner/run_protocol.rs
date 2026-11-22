//! 当前 run 的 RPC 参数检查与 engine 启动握手。

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::WorkflowError;
use crate::protocol::{
    AgentRunParams, AgentRunResult, AgentTaskParams, JournalEntry, ToolJournalEntry,
    ToolReplayedParams, ToolTaskParams, WorkflowDoneParams, WorkflowStartParams,
};
use crate::runner::artifact::{WORKFLOW_BUILD_ID, WORKFLOW_PROTOCOL_VERSION};
use crate::runner::WorkflowInput;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowStartAck {
    ok: bool,
    protocol_version: u32,
    build_id: String,
}

pub(super) fn validate_start_ack(value: Value) -> Result<(), WorkflowError> {
    let ack: WorkflowStartAck = serde_json::from_value(value).map_err(|_| {
        WorkflowError::SpawnFailed("workflow/start returned an invalid handshake".into())
    })?;
    if !ack.ok
        || ack.protocol_version != WORKFLOW_PROTOCOL_VERSION
        || ack.build_id != WORKFLOW_BUILD_ID
    {
        return Err(WorkflowError::SpawnFailed(format!(
            "workflow artifact protocol mismatch: expected protocol {WORKFLOW_PROTOCOL_VERSION} build {WORKFLOW_BUILD_ID}"
        )));
    }
    Ok(())
}

pub(super) trait RunScoped {
    fn run_id(&self) -> &str;
}

pub(super) fn parse_run_scoped<T: DeserializeOwned + RunScoped>(
    params: Option<Value>,
    expected_run_id: &str,
) -> Result<T, &'static str> {
    let parsed: T = serde_json::from_value(params.unwrap_or(Value::Null))
        .map_err(|_| "invalid run-scoped RPC parameters")?;
    if parsed.run_id() != expected_run_id {
        return Err("runId does not match the active workflow run");
    }
    Ok(parsed)
}

impl RunScoped for AgentRunParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}

impl RunScoped for AgentTaskParams {
    fn run_id(&self) -> &str {
        &self.params.run_id
    }
}

impl RunScoped for WorkflowDoneParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}

impl RunScoped for ToolReplayedParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}

/// 解析 `agent/task` 通知参数（design §5.2 第 1 条）。
///
/// 与 `agent/run` 同一条 run 作用域校验（`runId` 必须等于当前 run）；`taskId`
/// 非空——它是任务表主键的一半，空值会让任务不可寻址。
pub(super) fn parse_agent_task_params(
    params: Option<Value>,
    expected_run_id: &str,
) -> Result<(String, AgentRunParams), String> {
    let parsed: AgentTaskParams =
        parse_run_scoped(params, expected_run_id).map_err(str::to_string)?;
    if parsed.task_id.trim().is_empty() {
        return Err("taskId must be a non-empty string".to_string());
    }
    Ok((parsed.task_id, parsed.params))
}

/// `tool/task` 参数拒绝：typed 原因直接决定回填给 Node 的错误码。
#[derive(Debug, Clone)]
pub(super) struct ToolTaskRejection {
    pub(super) code: peri_acp_types::workflow::WorkflowToolErrorCode,
    pub(super) message: String,
}

impl ToolTaskRejection {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: peri_acp_types::workflow::WorkflowToolErrorCode::InvalidArguments,
            message: message.into(),
        }
    }

    fn not_executed(message: impl Into<String>) -> Self {
        Self {
            code: peri_acp_types::workflow::WorkflowToolErrorCode::NotExecuted,
            message: message.into(),
        }
    }
}

/// 解析 `tool/task` 通知参数（issue #407）。
///
/// 校验项与契约一致：`runId` 必须等于当前 run、`taskId` / `toolName` 非空、
/// `arguments` 必须是对象（省略 = 空对象）。这里只做形状检查，不重复 schema 校验
/// （归 MCP 服务端）。
pub(super) fn parse_tool_task_params(
    params: Option<Value>,
    expected_run_id: &str,
) -> Result<ToolTaskParams, ToolTaskRejection> {
    let Some(value) = params else {
        return Err(ToolTaskRejection::invalid("missing tool/task params"));
    };
    let run_id_matches = value.get("runId").and_then(Value::as_str) == Some(expected_run_id);
    let parsed: ToolTaskParams = serde_json::from_value(value).map_err(|_| {
        ToolTaskRejection::invalid(
            "invalid tool/task params: taskId/toolName must be strings and arguments an object",
        )
    })?;
    if parsed.task_id.trim().is_empty() {
        return Err(ToolTaskRejection::invalid(
            "taskId must be a non-empty string",
        ));
    }
    if parsed.tool_name.trim().is_empty() {
        return Err(ToolTaskRejection::invalid(
            "toolName must be a non-empty string",
        ));
    }
    if !run_id_matches {
        return Err(ToolTaskRejection::not_executed(
            "runId does not match the active workflow run",
        ));
    }
    Ok(parsed)
}

pub(super) fn workflow_start_params(
    run_id: &str,
    input: &WorkflowInput,
    resume: Option<Vec<JournalEntry>>,
    resume_tools: Option<Vec<ToolJournalEntry>>,
    cwd: &str,
) -> WorkflowStartParams {
    WorkflowStartParams {
        run_id: run_id.to_string(),
        script: input.script.clone(),
        args: input.args.clone(),
        budget_total: input.budget_total,
        max_concurrency: input.max_concurrency,
        limits: Some(input.limits.clone()),
        resume_from_run_id: input.resume_from.clone(),
        resume,
        resume_tools,
        cwd: cwd.to_string(),
    }
}

/// Resume only the deterministic prefix that was actually completed.
///
/// A dead/skipped result is a failed cache boundary: the engine cannot safely
/// reuse later entries because the script's subsequent call sequence depends on
/// the missing output. Keeping the prefix (rather than filtering individual
/// entries) makes the next call live and preserves cache identity for the
/// entries before it.
pub(super) fn reusable_journal_prefix(mut entries: Vec<JournalEntry>) -> Vec<JournalEntry> {
    entries.sort_by_key(|entry| entry.seq);
    let mut prefix = Vec::new();
    let paired_items = entries.into_iter().enumerate();
    for (expected_seq, entry) in paired_items {
        let expected_seq = u64::try_from(expected_seq).expect("journal prefix index fits in u64");
        if entry.seq != expected_seq || !matches!(entry.result, AgentRunResult::Ok { .. }) {
            break;
        }
        prefix.push(entry);
    }
    prefix
}

/// Resume only the deterministic prefix of replayed script tool calls.
///
/// 条目按 `tools.jsonl` 写入顺序消费（不重排）：前缀必须从 `toolSeq == 0` 起连续，
/// 且该调用发生前完成的 agent 调用数不得超过可回放的 agent 前缀长度。首个不满足
/// 即停止——之后的重放会与未重放的脚本执行顺序脱节，保留它们比丢弃更危险。
pub(super) fn replayable_tool_prefix(
    entries: Vec<ToolJournalEntry>,
    replayed_agent_prefix_len: u64,
) -> Vec<ToolJournalEntry> {
    let mut prefix = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let expected_seq = u64::try_from(index).expect("tool journal prefix index fits in u64");
        if entry.tool_seq != expected_seq || entry.agent_seq > replayed_agent_prefix_len {
            break;
        }
        prefix.push(entry);
    }
    prefix
}

// ─── Journal RPC 参数反序列化 ───────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JournalAppendParams {
    pub(super) run_id: String,
    pub(super) entry: crate::protocol::JournalEntry,
}

impl RunScoped for JournalAppendParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JournalTruncateParams {
    pub(super) run_id: String,
}

impl RunScoped for JournalTruncateParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}
