use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use rmcp::model::ReadResourceRequestParams;
use thiserror::Error;

use super::client::{ClientStatus, McpClientPool};
use crate::tools::output_persist::persist_truncated_output;

/// 资源读取工具错误
#[derive(Debug, Error)]
pub enum ResourceError {
    #[error("MCP 服务器 \"{server}\" 未找到")]
    ServerNotFound { server: String },
    #[error("MCP 服务器 \"{server}\" 未连接 (状态: {status:?})")]
    NotConnected {
        server: String,
        status: ClientStatus,
    },
    #[error("MCP 资源读取失败: {server}: {reason}")]
    ReadFailed { server: String, reason: String },
    #[error("MCP 资源读取参数错误: {0}")]
    InvalidParam(String),
}

const TOOL_NAME: &str = "mcp_read_resource";
const RESOURCE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_MCP_LINES: usize = 2000;

/// MCP 资源读取工具——统一资源读取入口
pub struct McpResourceTool {
    client_pool: Arc<McpClientPool>,
    cached_description: String,
}

impl McpResourceTool {
    pub fn new(client_pool: Arc<McpClientPool>) -> Self {
        let summary = client_pool.resource_summary();
        let cached_description = if summary.is_empty() {
            "Read a resource from an MCP server. No resources currently available.".to_string()
        } else {
            format!(
                "Read a resource from an MCP server. Available resources:\n{}",
                summary
            )
        };
        Self {
            client_pool,
            cached_description,
        }
    }
}

#[async_trait]
impl BaseTool for McpResourceTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server_name": {
                    "type": "string",
                    "description": "MCP 服务器名称（配置中的 key）"
                },
                "uri": {
                    "type": "string",
                    "description": "要读取的资源 URI"
                }
            },
            "required": ["server_name", "uri"]
        })
    }

    fn description(&self) -> &str {
        &self.cached_description
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // 1. 提取参数
        let server_name = input
            .get("server_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ResourceError::InvalidParam("缺少 server_name 参数".into()))?;
        let uri = input
            .get("uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ResourceError::InvalidParam("缺少 uri 参数".into()))?;

        // 2. 获取客户端句柄
        let handle = self
            .client_pool
            .get_client(server_name)
            .ok_or_else(|| ResourceError::ServerNotFound {
                server: server_name.to_string(),
            })?
            .clone();

        // 3. 检查连接状态
        if !matches!(handle.status, ClientStatus::Connected) {
            return Err(Box::new(ResourceError::NotConnected {
                server: server_name.to_string(),
                status: handle.status.clone(),
            }));
        }

        let peer = handle
            .peer
            .as_ref()
            .ok_or_else(|| ResourceError::NotConnected {
                server: server_name.to_string(),
                status: ClientStatus::Disconnected,
            })?;

        // 4. 调用 rmcp read_resource
        let request = ReadResourceRequestParams::new(uri);
        let result = tokio::time::timeout(RESOURCE_READ_TIMEOUT, peer.read_resource(request)).await;

        match result {
            Ok(Ok(resource_result)) => {
                // 5. 格式化资源内容（截断超大输出）
                let mut output = Vec::new();
                for content in &resource_result.contents {
                    match content {
                        rmcp::model::ResourceContents::TextResourceContents {
                            text,
                            mime_type,
                            ..
                        } => {
                            let mime = mime_type.as_deref().unwrap_or("plain");
                            output.push(format!("[text/{}]", mime));
                            output.push(text.clone());
                        }
                        rmcp::model::ResourceContents::BlobResourceContents {
                            blob,
                            mime_type,
                            ..
                        } => {
                            let mime = mime_type.as_deref().unwrap_or("octet-stream");
                            output.push(format!("[blob/{}]", mime));
                            output.push(format!("<{} bytes of binary data>", blob.len()));
                        }
                    }
                }
                let formatted = output.join("\n");
                let lines: Vec<&str> = formatted.lines().collect();
                let result = if lines.len() > MAX_MCP_LINES {
                    let persist_hint = persist_truncated_output(&formatted);
                    let truncated: String = lines[..MAX_MCP_LINES].join("\n");
                    format!(
                        "{truncated}\n\n[MCP output truncated: {} total lines]{persist_hint}",
                        lines.len()
                    )
                } else {
                    formatted
                };
                Ok(result)
            }
            Ok(Err(e)) => Err(Box::new(ResourceError::ReadFailed {
                server: server_name.to_string(),
                reason: e.to_string(),
            })),
            Err(_) => Err(Box::new(ResourceError::ReadFailed {
                server: server_name.to_string(),
                reason: format!("资源读取超时 ({}s)", RESOURCE_READ_TIMEOUT.as_secs()),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("resource_tool_test.rs");
}
