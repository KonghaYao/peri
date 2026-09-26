//! `web` builtin MCP 实例（真实 `ServerHandler`，owner I-01）。
//!
//! 复用既有工具实现（`crate::middleware::web_search` / `web_fetch`），**不重写**
//! 网络语义与 schema：handler 只负责
//! ① `tools/list` 由 `BaseTool::definition()` 映射、② `tools/call` 按名路由到
//! `BaseTool::invoke`、③ 结果按 IF-D14 冻结的三种形态映射。
//!
//! 硬约束（主 plan §10 R2）：**不得**覆写 `discover`。rmcp 默认实现走 modern
//! （inline `server/discover`）握手；覆写会退回 legacy 并让同一连接上的 `tools/list`
//! 被 server 会话层以 `-32602` 拒绝（`builtin_spike_test.rs` Q1(b)/Q3(b) 的线路级证据）。
//!
//! 本模块同时是三个 handler 共用的映射助手与实例工厂的落点（`artifact.rs` 复用，
//! 不另写第二份映射）：
//! - [`rmcp_tool_from_base`] / [`list_tools_of`]：`BaseTool` → `rmcp::model::Tool`
//! - [`invoke_tool_call`]：IF-D14 结果映射（未知工具名 / 成功 / 失败三种形态）
//! - [`builtin_server_handler`]：实例名 → handler 的唯一入口（`mcp::builtin::runtime`
//!   的 `spawn_builtin_transport` 消费）

use std::{path::Path, sync::Arc};

use peri_acp_types::builtin_mcp::find;
use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};
use serde_json::Value;

use super::artifact::ArtifactMcpServer;
use crate::middleware::{web_fetch::WebFetchTool, web_search::WebSearchTool};

/// `web` 实例的 `ServerInfo` 名字（`Implementation::name`）。
const WEB_SERVER_NAME: &str = "peri-web-mcp";

/// IF-D14 失败形态的**固定规则文本**：只含工具名，不含路径 / env / 凭据
/// （§9 规则 7）。原始错误（可能内嵌文件路径或 URL）不入模型面文本。
fn execution_failure_text(tool: &str) -> String {
    format!(
        "tool `{tool}` failed to execute; the failure detail is withheld by policy \
         (no paths, environment values, or credentials are exposed). Verify the input and retry."
    )
}

/// `ServerInfo`：只声明 tools 能力（resources / logging 不声明，沿用既有降级行为）。
pub(crate) fn server_info(name: &'static str) -> ServerInfo {
    ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new(name, env!("CARGO_PKG_VERSION")))
}

/// `BaseTool::definition()` → `rmcp::model::Tool`（两个实例**共用同一份**，不另写）。
///
/// `input_schema` 必须是 JSON object：该结构约束与
/// `mcp::system_tools::validate_input_schema`（启动期 `SystemToolError::InvalidSchema`
/// 的来源）同源；`parameters()` 不是 object 的工具会在启动期被拒绝，因此这里保持
/// object 映射（空 object 兜底，不 panic）。
pub(crate) fn rmcp_tool_from_base(tool: &dyn BaseTool) -> Tool {
    let definition = tool.definition();
    let schema = definition
        .parameters
        .as_object()
        .cloned()
        .unwrap_or_default();
    Tool::new(definition.name, definition.description, schema)
}

/// `tools/list` 结果：声明顺序 = `tools` 顺序（注册表声明的顺序）。
pub(crate) fn list_tools_of(tools: &[Arc<dyn BaseTool>]) -> ListToolsResult {
    ListToolsResult::with_all_items(
        tools
            .iter()
            .map(|tool| rmcp_tool_from_base(tool.as_ref()))
            .collect(),
    )
}

/// IF-D14 的**唯一**实现：`tools/call` 的结果映射。
///
/// | 情形 | 返回 |
/// | --- | --- |
/// | 工具名不在本实例工具集内 | `Err(McpError::invalid_params("unknown tool: {name}"))` |
/// | 工具执行 `Ok(text)` | `Ok(Complete(CallToolResult::success([text])))` |
/// | 工具执行 `Err(_)` | `Ok(Complete(CallToolResult::error([固定规则文本])))` |
///
/// 取消 / 超时与 `Err` 同路（工具返回 `Err`，例如 reqwest 请求被取消）：进模型可见的
/// error 结果，**不** panic、**不** abort，也**不**用 `McpError::internal_error`。
/// 该路径的失败是**调用期**错误；启动期失败走 readiness 的 fatal 路径（两者不同层）。
pub(crate) async fn invoke_tool_call(
    tools: &[Arc<dyn BaseTool>],
    cwd: &str,
    request: &CallToolRequestParams,
) -> Result<CallToolResponse, McpError> {
    let Some(tool) = tools
        .iter()
        .find(|tool| tool.name() == request.name.as_ref())
    else {
        return Err(McpError::invalid_params(
            format!("unknown tool: {}", request.name),
            None,
        ));
    };

    // 参数缺省 = 空对象（工具按 `input["field"].as_str()` 取参，缺失即报 invalid input）。
    let input = Value::Object(request.arguments.clone().unwrap_or_default());
    match tool.invoke(input, ToolContext::new(&[], cwd)).await {
        Ok(text) => Ok(CallToolResponse::Complete(CallToolResult::success(vec![
            ContentBlock::text(text),
        ]))),
        Err(_error) => Ok(CallToolResponse::Complete(CallToolResult::error(vec![
            ContentBlock::text(execution_failure_text(tool.name())),
        ]))),
    }
}

/// `web` 实例的 handler：持有 `WebSearch` / `WebFetch` 两个既有工具。
///
/// Web 工具无状态、无凭据、无 capability root（`cwd` 不被读取），因此 handler 是纯函数式的。
pub(crate) struct WebMcpServer {
    tools: Vec<Arc<dyn BaseTool>>,
}

impl WebMcpServer {
    /// 生产构造：工具顺序与注册表声明顺序一致（`WebSearch` → `WebFetch`）。
    pub(crate) fn new() -> Self {
        let tools: Vec<Arc<dyn BaseTool>> = vec![
            Arc::new(WebSearchTool::new()),
            Arc::new(WebFetchTool::new()),
        ];
        Self { tools }
    }

    /// 测试构造：注入替身工具（MCP 链路两侧都真实，只有工具结果是受控的）。
    ///
    /// Web 两个工具的后端地址是编译期常量，无法在单测内指向本地桩，因此成功 / 失败
    /// 两种形态只能由替身工具产生；生产构造 [`Self::new`] 的工具集另有断言覆盖。
    #[cfg(test)]
    pub(crate) fn with_tools(tools: Vec<Arc<dyn BaseTool>>) -> Self {
        Self { tools }
    }

    fn tools(&self) -> &[Arc<dyn BaseTool>] {
        &self.tools
    }
}

impl ServerHandler for WebMcpServer {
    // 不覆写 `discover`（§10 R2：覆写会让 `tools/list` 被会话层拒绝）。

    fn get_info(&self) -> ServerInfo {
        server_info(WEB_SERVER_NAME)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(list_tools_of(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        // Web 工具忽略 `cwd`（`web_search.rs` / `web_fetch.rs` 的 `invoke` 不读 `ToolContext`）。
        invoke_tool_call(self.tools(), "", &request).await
    }
}

/// 两个实例的 handler 类型擦除：`runtime` 需要在运行时按实例名选择 handler，
/// 而 `rmcp::serve_server` 要求泛型 `S: ServerHandler`（`Arc<dyn ServerHandler>`
/// 不满足该约束），因此用枚举分派。
pub(crate) enum BuiltinServerHandler {
    Web(WebMcpServer),
    Artifact(ArtifactMcpServer),
}

impl ServerHandler for BuiltinServerHandler {
    // 不覆写 `discover`（§10 R2）。

    fn get_info(&self) -> ServerInfo {
        match self {
            Self::Web(server) => server.get_info(),
            Self::Artifact(server) => server.get_info(),
        }
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        match self {
            Self::Web(server) => server.list_tools(request, context).await,
            Self::Artifact(server) => server.list_tools(request, context).await,
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        match self {
            Self::Web(server) => server.call_tool(request, context).await,
            Self::Artifact(server) => server.call_tool(request, context).await,
        }
    }
}

/// 实例名 → handler 工厂（`mcp::builtin::runtime` 的 `spawn_builtin_transport`
/// 的唯一入口；`None` = 非已实现实例，调用方按 `Failed` 收口）。
///
/// `runtime.rs` 的接线形如：
///
/// ```ignore
/// let handler = super::web::builtin_server_handler(instance, cwd)
///     .ok_or_else(|| BuiltinSpawnError::HandlerNotWired { instance: instance.to_string() })?;
/// spawn_builtin_transport_with_handler(instance, handler)
/// ```
///
/// 名字到模块的分派是**代码**事实（模块不能由数据构造）；实例名 / 工具名 /
/// `direct` / 保留名的唯一事实源仍是注册表（`peri_acp_types::builtin_mcp`）。
/// `cwd` 只被 `artifact` 实例用作相对路径解析根（不是安全沙箱），`web` 忽略它。
pub(crate) fn builtin_server_handler(instance: &str, cwd: &Path) -> Option<BuiltinServerHandler> {
    match find(instance)?.name {
        "web" => Some(BuiltinServerHandler::Web(WebMcpServer::new())),
        "artifact" => Some(BuiltinServerHandler::Artifact(ArtifactMcpServer::new(cwd))),
        _ => None,
    }
}

#[cfg(test)]
#[path = "web_test.rs"]
mod tests;
