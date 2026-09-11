//! 当前 run 的 RPC 参数检查与 engine 启动握手。

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::artifact::{WORKFLOW_BUILD_ID, WORKFLOW_PROTOCOL_VERSION};
use super::WorkflowInput;
use crate::error::WorkflowError;
use crate::protocol::{AgentRunParams, JournalEntry, WorkflowDoneParams, WorkflowStartParams};

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

impl RunScoped for WorkflowDoneParams {
    fn run_id(&self) -> &str {
        &self.run_id
    }
}

pub(super) fn parse_agent_run_params(
    params: Option<Value>,
    expected_run_id: &str,
) -> Result<AgentRunParams, String> {
    parse_run_scoped(params, expected_run_id).map_err(str::to_string)
}

pub(super) fn workflow_start_params(
    run_id: &str,
    input: &WorkflowInput,
    resume: Option<Vec<JournalEntry>>,
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
        cwd: cwd.to_string(),
    }
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
