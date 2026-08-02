//! JSON-RPC 2.0 消息类型，对齐 spec 第 3 节 RPC 协议。
//!
//! 帧格式：newline-delimited JSON（每行一条消息，`\n` 分隔）。
//! stdout 传 JSON-RPC，stderr 留给 Node console.error。
//!
//! ⚠ 跨侧契约：本文件的 wire 字段（WorkflowStartParams / AgentRunParams 等）
//! 与 npm 侧 `npm-packages/@peri-workflow/src/types.ts` 保持同步，变更须两侧一致
//! （npm 侧文件顶部有对应注释）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── Rust → Node 请求 ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStartParams {
    pub run_id: String,
    pub script: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    pub budget_total: Option<u64>, // null = 无限
    pub max_concurrency: u32,
    pub resume: Option<Vec<JournalEntry>>, // 非-null 时携带 journal entries
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowKillParams {
    pub run_id: String,
}

// ─── Node → Rust 请求（agent 回调）──────────────────────────

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

// ─── AgentRunResult（对齐引擎 AgentRunResult）──────────────

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
}

// ─── ProgressEvent（对齐引擎 ProgressEvent 8 种类型）────────

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
        token_count: u64,
        tool_count: u64,
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

// ─── Journal ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub key: String,
    pub seq: u64,
    pub result: AgentRunResult,
}

// ─── WorkflowDone ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDoneParams {
    pub run_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ─── 通用 JSON-RPC 消息封装 ────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<u64>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error codes
pub const ERR_ABORTED: i32 = -32000;
pub const ERR_INTERNAL: i32 = -32603;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
