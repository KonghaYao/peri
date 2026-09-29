use std::sync::Arc;

use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, ServerCapabilities, ServerInfo, Tool,
    },
    ErrorData as McpError,
};
use serde_json::Value;

/// `ServerInfo` with only tool capability enabled and the caller's package version.
pub fn server_info(name: &'static str, version: &'static str) -> ServerInfo {
    ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new(name, version))
}

/// Convert the canonical `BaseTool` definition into an rmcp tool schema.
pub fn rmcp_tool_from_base(tool: &dyn BaseTool) -> Tool {
    let definition = tool.definition();
    let schema = definition
        .parameters
        .as_object()
        .cloned()
        .unwrap_or_default();
    Tool::new(definition.name, definition.description, schema)
}

/// Build a tools/list response while preserving the declared tool order.
pub fn list_tools_of(tools: &[Arc<dyn BaseTool>]) -> ListToolsResult {
    ListToolsResult::with_all_items(
        tools
            .iter()
            .map(|tool| rmcp_tool_from_base(tool.as_ref()))
            .collect(),
    )
}

/// Invoke a registered tool and project failures through the shared safe diagnostic policy.
pub async fn invoke_tool_call(
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

    let input = Value::Object(request.arguments.clone().unwrap_or_default());
    match tool.invoke(input, ToolContext::new(&[], cwd)).await {
        Ok(text) => Ok(CallToolResponse::Complete(CallToolResult::success(vec![
            ContentBlock::text(text),
        ]))),
        Err(error) => Ok(CallToolResponse::Complete(CallToolResult::error(vec![
            ContentBlock::text(crate::result_mapping::failure_text(
                tool.name(),
                error.as_ref(),
            )),
        ]))),
    }
}
