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

// ─── Agent 回调协议类型（3.0 批 2 波 1 迁入 peri-acp-types）──────────
//
// `AgentRunParams` / `AgentRunResult` / `Usage` / `ProgressEvent` 为协议纯类型，
// 已迁入契约层 `peri_acp_types::workflow`；本模块保留 re-export 保兼容
// （npm 侧 wire 字段契约不变，见文件头注释）。

pub use peri_acp_types::workflow::{AgentRunParams, AgentRunResult, ProgressEvent, Usage};

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
pub const ERR_INVALID_PARAMS: i32 = -32602;
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
