//! 工具结果的保真契约：一次 canonical 工具执行的 raw 结果与远端错误载荷。
//!
//! 本模块是 raw 工具结果的**唯一权威定义**。MCP `tools/call` 结果（`content`
//! 多模态块、`structuredContent`、`_meta`、`isError` 及未知扩展字段）按线上 JSON
//! 保真承载，不因协议演进或未知扩展丢字段；同时不把 rmcp engine 依赖注入契约层
//! 与 Agent 层。
//!
//! 结果文本（模型/UI 投影）与 raw 结果分离：文本是可截断的投影，raw 是事实源。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP `tools/call` 结果的权威 raw 表示。
///
/// 内部以 [`Value`] 保真承载，序列化形状即 MCP 结果对象本身：`content`（含
/// `text`/`image`/`resource` 等多模态块）、`structuredContent`、`_meta`、
/// `isError`，以及任何未知扩展字段。缺失的字段往返后仍然缺失——本类型不补全、
/// 不重命名、不丢弃。
///
/// 与逐字段结构体（旧 `RawCallToolResult`）的差异：单个字段形状意外（例如
/// `content` 不是数组）不再让整份结果退化为解析失败，raw 载荷始终可用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawToolResult(Value);

/// [`serde_json::Value`] 不实现 `Eq`，因为 `f64` 的相等性在 NaN 上非自反；但
/// serde_json 拒绝非有限浮点（`Number::from_f64` 对 NaN/Infinity 返回 `None`，
/// 反序列化同样拒绝），构造不出 NaN，故自反性成立。手动实现 `Eq` 让
/// [`crate::tools::ToolOutput`] 等上层契约类型不必降级比较语义。
impl Eq for RawToolResult {}

impl RawToolResult {
    pub fn new(value: Value) -> Self {
        Self(value)
    }

    /// 底层 JSON 原文（只读）。
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// 取出底层 JSON 原文。
    pub fn into_value(self) -> Value {
        self.0
    }

    /// 工具是否在带内报告失败（MCP `isError`）。缺省 `false`：字段缺失即未报告
    /// 失败，不代表工具成功。
    pub fn is_error(&self) -> bool {
        self.0
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// `content` 块（含多模态块）。字段缺失或形状意外时返回空切片。
    pub fn content(&self) -> &[Value] {
        self.0
            .get("content")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// `structuredContent`（结构化结果）；未提供时为 `None`。
    pub fn structured_content(&self) -> Option<&Value> {
        self.0.get("structuredContent").filter(|v| !v.is_null())
    }

    /// `_meta` 扩展元数据；未提供时 `None`。未知键按原文保留。
    pub fn meta(&self) -> Option<&serde_json::Map<String, Value>> {
        self.0.get("_meta").and_then(Value::as_object)
    }
}

impl From<Value> for RawToolResult {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl From<RawToolResult> for Value {
    fn from(result: RawToolResult) -> Self {
        result.0
    }
}

/// JSON-RPC 2.0 错误对象（`code` / `message` / `data`）。
///
/// MCP 远端错误的线上形状；MCP Apps relay envelope 复用同一份定义
/// （`crate::mcp_apps::JsonRpcError` 为其 re-export），同一 wire 概念不留两份类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// 远端 MCP 返回的 raw 错误事实。
///
/// 两类载荷分开表达，relay/ACP 不再靠 error code 反推远端语义：
/// - 协议层失败：JSON-RPC `error` 对象（[`JsonRpcError`]）；
/// - 带内失败：调用本身成功，但工具以 `isError: true` 报告结果（[`RawToolResult`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteToolError {
    /// 协议层错误；`None` = 失败不来自 JSON-RPC error 响应。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<JsonRpcError>,
    /// 带内失败载荷；`None` = 失败不来自工具结果本身。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_band: Option<RawToolResult>,
}

impl RemoteToolError {
    /// 协议层错误（JSON-RPC `error` 响应）。
    pub fn from_jsonrpc(error: JsonRpcError) -> Self {
        Self {
            jsonrpc: Some(error),
            in_band: None,
        }
    }

    /// 带内失败（工具有结果载荷但 `isError: true`）。
    pub fn from_in_band(result: RawToolResult) -> Self {
        Self {
            jsonrpc: None,
            in_band: Some(result),
        }
    }
}

#[cfg(test)]
#[path = "tool_outcome_test.rs"]
mod tests;
