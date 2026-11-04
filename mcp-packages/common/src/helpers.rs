use std::sync::Arc;

use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        ListToolsResult, ServerCapabilities, ServerConfig, Tool,
    },
    ErrorData as McpError,
};
use serde_json::Value;

/// `ServerConfig` with only tool capability enabled and the caller's package version.
pub fn server_info(name: &'static str, version: &'static str) -> ServerConfig {
    ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
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

/// Invoke a registered tool and preserve diagnostic detail in tool failures.
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
    match tool.invoke_output(input, ToolContext::new(&[], cwd)).await {
        Ok(output) => {
            // raw 中带图片时保留多模态 content（Read 的 png/jpeg/gif/webp）；
            // 其余工具仍走既有纯文本投影。
            let content = raw_image_blocks(&output)
                .unwrap_or_else(|| vec![ContentBlock::text(output.text.clone())]);
            let is_error = output.is_error() == Some(true);
            Ok(CallToolResponse::Complete(
                crate::result_mapping::with_execution(
                    if is_error {
                        CallToolResult::error(content)
                    } else {
                        CallToolResult::success(content)
                    },
                    output.execution.as_ref(),
                ),
            ))
        }
        Err(error) => {
            let text = crate::result_mapping::failure_text(tool.name(), error.as_ref());
            tracing::warn!(tool = tool.name(), diagnostic = %text, "MCP tool call failed");
            Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                ContentBlock::text(text),
            ])))
        }
    }
}

/// `ToolOutput` raw content 含图片块时，把图片提升为 MCP content，文本沿用既有投影。
///
/// 只解析图片块：文本块的解析失败不会连累图片；任一图片块无法解析时整体回退为
/// 纯文本投影，避免模型收到不完整的多模态结果。
fn raw_image_blocks(output: &peri_agent::tools::ToolOutput) -> Option<Vec<ContentBlock>> {
    let raw = output.raw()?;
    let mut images = Vec::new();
    for block in raw.content() {
        if block.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        let image = serde_json::from_value::<ContentBlock>(block.clone()).ok()?;
        images.push(image);
    }
    if images.is_empty() {
        return None;
    }
    let mut content = Vec::with_capacity(images.len() + 1);
    if !output.text.is_empty() {
        content.push(ContentBlock::text(output.text.clone()));
    }
    content.extend(images);
    Some(content)
}
