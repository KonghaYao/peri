use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use rmcp::model::{Content, Tool};
use thiserror::Error;

use super::client::{McpClientHandle, McpClientPool};
use crate::tools::output_persist::persist_truncated_output;

/// MCP 工具调用错误
#[derive(Debug, Error)]
pub enum ToolCallError {
    #[error("MCP 服务器 \"{server}\" 未连接 (状态: {status:?})")]
    NotConnected { server: String, status: String },
    #[error("MCP 服务器 \"{server}\" 工具 \"{tool}\" 调用失败: {reason}")]
    CallFailed {
        server: String,
        tool: String,
        reason: String,
    },
    #[error("MCP 服务器 \"{server}\" 工具 \"{tool}\" 调用超时 ({timeout_secs}s)")]
    Timeout {
        server: String,
        tool: String,
        timeout_secs: u64,
    },
}

/// 将单个 MCP tool 包装为 BaseTool 实现
pub struct McpToolBridge {
    server_name: String,
    tool_name: String,
    full_name: String,
    description: String,
    input_schema: serde_json::Value,
    client: Arc<McpClientHandle>,
}

const TOOL_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_MCP_LINES: usize = 2000;

/// Sanitize name components to match API tool name pattern: ^[a-zA-Z0-9_-]+$
fn sanitize_name_component(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl McpToolBridge {
    pub fn new(server_name: &str, tool: &Tool, client: Arc<McpClientHandle>) -> Self {
        let tool_name = tool.name.to_string();
        let full_name = format!(
            "mcp__{}__{}",
            sanitize_name_component(server_name),
            sanitize_name_component(&tool_name)
        );
        let description = format!(
            "[MCP:{}] {}",
            server_name,
            tool.description.as_ref().map(|d| d.as_ref()).unwrap_or("")
        );
        let input_schema = serde_json::to_value(&*tool.input_schema)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        Self {
            server_name: server_name.to_string(),
            tool_name,
            full_name,
            description,
            input_schema,
            client,
        }
    }
}

#[async_trait]
impl BaseTool for McpToolBridge {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // 1. 检查连接状态
        match &self.client.peer {
            Some(_) => {}
            None => {
                return Err(Box::new(ToolCallError::NotConnected {
                    server: self.server_name.clone(),
                    status: format!("{:?}", self.client.status),
                }));
            }
        }

        let peer = self.client.peer.as_ref().unwrap();

        // 2. 构建 rmcp 请求参数
        let arguments = input.as_object().cloned().unwrap_or_default();
        let request = rmcp::model::CallToolRequestParams::new(self.tool_name.clone())
            .with_arguments(arguments);

        // 3. 带超时调用 peer.call_tool()
        let result = tokio::time::timeout(TOOL_CALL_TIMEOUT, peer.call_tool(request))
            .await
            .map_err(|_| ToolCallError::Timeout {
                server: self.server_name.clone(),
                tool: self.tool_name.clone(),
                timeout_secs: TOOL_CALL_TIMEOUT.as_secs(),
            })?
            .map_err(|e| ToolCallError::CallFailed {
                server: self.server_name.clone(),
                tool: self.tool_name.clone(),
                reason: e.to_string(),
            })?;

        // 4. 处理 is_error 标志
        if result.is_error.unwrap_or(false) {
            let error_text = format_contents(&result.content);
            let lines: Vec<&str> = error_text.lines().collect();
            let reason = if lines.len() > MAX_MCP_LINES {
                let persist_hint = persist_truncated_output(&error_text);
                let truncated: String = lines[..MAX_MCP_LINES].join("\n");
                format!(
                    "{truncated}\n\n[MCP error output truncated: {} total lines]{persist_hint}",
                    lines.len()
                )
            } else {
                error_text
            };
            return Err(Box::new(ToolCallError::CallFailed {
                server: self.server_name.clone(),
                tool: self.tool_name.clone(),
                reason,
            }));
        }

        // 5. 格式化返回（截断超大输出）
        let formatted = format_contents(&result.content);
        let lines: Vec<&str> = formatted.lines().collect();
        let output = if lines.len() > MAX_MCP_LINES {
            let persist_hint = persist_truncated_output(&formatted);
            let truncated: String = lines[..MAX_MCP_LINES].join("\n");
            format!(
                "{truncated}\n\n[MCP output truncated: {} total lines]{persist_hint}",
                lines.len()
            )
        } else {
            formatted
        };
        Ok(output)
    }
}

/// 将 content 列表格式化为纯文本字符串
fn format_contents(contents: &[Content]) -> String {
    let mut parts = Vec::new();
    for content in contents {
        match &content.raw {
            rmcp::model::RawContent::Text(text_content) => {
                parts.push(text_content.text.clone());
            }
            rmcp::model::RawContent::Image(image_content) => {
                parts.push(format!("[image: {}]", image_content.mime_type));
            }
            rmcp::model::RawContent::Resource(resource_content) => {
                let uri = match &resource_content.resource {
                    rmcp::model::ResourceContents::TextResourceContents { uri, .. } => uri.clone(),
                    rmcp::model::ResourceContents::BlobResourceContents { uri, .. } => uri.clone(),
                };
                parts.push(format!("[resource: {}]", uri));
            }
            rmcp::model::RawContent::Audio(audio_content) => {
                parts.push(format!("[audio: {}]", audio_content.mime_type));
            }
            rmcp::model::RawContent::ResourceLink(link) => {
                parts.push(format!("[resource_link: {}]", link.uri));
            }
        }
    }
    parts.join("\n")
}

/// 从 McpClientPool 的所有已连接客户端中批量创建 McpToolBridge
pub fn build_tool_bridges(pool: &McpClientPool) -> Vec<Box<dyn BaseTool>> {
    let mut bridges: Vec<Box<dyn BaseTool>> = Vec::new();
    for client in pool.get_all_clients() {
        for tool in &client.tools {
            bridges.push(Box::new(McpToolBridge::new(
                &client.name,
                tool,
                Arc::clone(&client),
            )));
        }
    }
    bridges
}

/// 统一工具池组装：内置工具优先去重

#[cfg(test)]
#[path = "tool_bridge_test.rs"]
mod tests;
