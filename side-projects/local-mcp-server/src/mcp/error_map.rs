//! 协议级错误与工具结果的**唯一**映射入口（WP-005）。
//!
//! MCP 把失败分成两类，混淆会让调用方看不到真正的原因：
//!
//! - **工具业务错误**：输入校验失败、文件不存在、命令非零退出、越界拒绝。工具跑了
//!   （或尝试跑了）并给出结果，调用方应看到文本 ⇒ [`ToolResponse::is_error`] 为真的
//!   tool result（规范 SEP-1303）。
//! - **JSON-RPC 协议错误**：请求本身无法路由或本身不合法（未知工具、请求形状损坏、
//!   服务器内部失败/后端不可用）⇒ [`ErrorData`]。
//!
//! 错误码只从 [`crate::error::ToolError`] 派生，禁止在本模块之外拼字面量；`-32002`
//! 在本协议版本必须**不得**发出（见 `crate::error::code`）。

use rmcp::model::{CallToolResult, ContentBlock, ErrorCode, ErrorData, Implementation, MetaObject};
use serde_json::{Map, Value};

use crate::error::ToolError;
use crate::wire::{ToolResponse, META_KEY_SERVER_INFO};

/// JSON 对象别名（`_meta` 与 `inputSchema` 都用它）。
type JsonObject = Map<String, Value>;

/// 由共享错误码构造 JSON-RPC 错误（唯一入口）。
pub fn rpc_error(code: i32, message: impl Into<String>) -> ErrorData {
    ErrorData::new(ErrorCode(code), message.into(), None)
}

/// 同上，但带结构化 `data`。
///
/// 资源类错误的 `data` 里回显调用方已提交的 `uri`（规范 `server/resources#error-handling`
/// 的示例形状）。回显的不是解析结果，因此不泄露任何服务端路径或他人信息。
pub fn rpc_error_with_data(code: i32, message: impl Into<String>, data: Value) -> ErrorData {
    ErrorData::new(ErrorCode(code), message.into(), Some(data))
}

/// [`ToolError`] → JSON-RPC 错误。
pub fn mcp_error(error: &ToolError) -> ErrorData {
    let payload = error.to_payload();
    ErrorData::new(ErrorCode(payload.code), payload.message, payload.data)
}

/// 构造结果级 `_meta`：先放工具自有元数据，最后写入服务端身份。
///
/// 顺序是刻意的：`serverInfo` 必须由服务端最终决定，工具**不得**覆盖它，否则工具
/// 可以冒充服务端身份。规范要求结果 SHOULD 携带 `io.modelcontextprotocol/serverInfo`。
pub fn meta_with_server_info(
    extra: Option<&JsonObject>,
    server_info: &Implementation,
) -> MetaObject {
    let mut map = JsonObject::new();
    if let Some(extra) = extra {
        for (key, value) in extra {
            map.insert(key.clone(), value.clone());
        }
    }
    let server_info = serde_json::to_value(server_info)
        .expect("Implementation 的序列化不可能失败（全部为字符串/可选字段）");
    map.insert(META_KEY_SERVER_INFO.to_string(), server_info);
    MetaObject(map)
}

/// [`ToolResponse`] → 工具调用结果。
///
/// 文本与结构化内容必须描述**同一个事实**：本函数只做搬运，不加工、不裁剪，
/// 因此"文本说了什么、结构里就有什么"由工具实现一次性保证（FC-MCP-02）。
/// `content` 为空但 `structuredContent` 存在是规范允许的（结构化专用结果），
/// 因此空文本不补齐占位文本，避免制造不存在的输出。
pub fn call_tool_result(response: &ToolResponse, server_info: &Implementation) -> CallToolResult {
    let mut result = if response.is_error {
        CallToolResult::error(Vec::new())
    } else {
        CallToolResult::success(Vec::new())
    };
    if !response.text.is_empty() {
        result.content = vec![ContentBlock::text(response.text.clone())];
    }
    result.structured_content = Some(response.structured.clone());
    result.is_error = Some(response.is_error);
    result.meta = Some(meta_with_server_info(response.meta.as_ref(), server_info));
    result
}

#[cfg(test)]
#[path = "error_map_test.rs"]
mod tests;
