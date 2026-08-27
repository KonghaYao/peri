use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::{
    dynamic_mcp::{CanonicalDynamicMcpAction, DynamicMcpAction},
    ports::DynamicMcpDeploymentPort,
    tools::{BaseTool, BoundToolInvocation, ToolContext},
};
use peri_agent::middleware::r#trait::Middleware;
use serde_json::{json, Value};

pub const DYNAMIC_MCP_TOOL_NAME: &str = "DynamicMCP";

#[derive(Debug, thiserror::Error)]
#[error("Dynamic MCP operation failed: {0}")]
struct DynamicMcpToolError(String);

/// Deferred session-scoped Dynamic MCP control tool.
pub struct DynamicMcpTool {
    session_id: String,
    deployment: Arc<dyn DynamicMcpDeploymentPort>,
}

impl DynamicMcpTool {
    pub fn new(
        session_id: impl Into<String>,
        deployment: Arc<dyn DynamicMcpDeploymentPort>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            deployment,
        }
    }
}

pub struct DynamicMcpMiddleware {
    tool: Arc<DynamicMcpTool>,
}

impl DynamicMcpMiddleware {
    pub fn new(
        session_id: impl Into<String>,
        deployment: Arc<dyn DynamicMcpDeploymentPort>,
    ) -> Self {
        Self {
            tool: Arc::new(DynamicMcpTool::new(session_id, deployment)),
        }
    }
}

#[async_trait]
impl Middleware for DynamicMcpMiddleware {
    fn name(&self) -> &str {
        "DynamicMcpMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(DynamicMcpTool::new(
            self.tool.session_id.clone(),
            Arc::clone(&self.tool.deployment),
        ))]
    }
}

struct BoundDynamicMcpTool {
    session_id: String,
    deployment: Arc<dyn DynamicMcpDeploymentPort>,
    action: CanonicalDynamicMcpAction,
    policy_name: &'static str,
}

#[async_trait]
impl BaseTool for BoundDynamicMcpTool {
    fn name(&self) -> &str {
        self.policy_name
    }

    fn description(&self) -> &str {
        "A canonical Dynamic MCP operation bound before approval."
    }

    fn parameters(&self) -> Value {
        json!({"type": "object"})
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.deployment
            .execute(&self.session_id, self.action.clone())
            .await
            .and_then(|response| {
                serde_json::to_string(&response).map_err(|_| {
                    peri_acp_types::dynamic_mcp::DynamicMcpFailure::new(
                        peri_acp_types::dynamic_mcp::DynamicMcpErrorCode::Internal,
                        peri_acp_types::dynamic_mcp::DynamicMcpOperationState::Failed,
                        "Dynamic MCP response serialization failed",
                    )
                })
            })
            .map_err(|failure| {
                Box::new(DynamicMcpToolError(format!(
                    "{}: {}",
                    failure.code.as_str(),
                    failure.safe_summary
                ))) as Box<dyn std::error::Error + Send + Sync>
            })
    }
}

#[async_trait]
impl BaseTool for DynamicMcpTool {
    fn name(&self) -> &str {
        DYNAMIC_MCP_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Load, inspect or unload an MCP server for this session. DynamicMCP is deferred-only. Mutating methods require user approval."
    }

    fn parameters(&self) -> Value {
        let secret_ref = json!({
            "type": "object",
            "properties": {"secretRef": {"type": "string"}},
            "required": ["secretRef"],
            "additionalProperties": false
        });
        let subscriptions = json!({
            "type": "object",
            "properties": {
                "resources": {"type": "array", "items": {"type": "string"}},
                "toolsListChanged": {"type": "boolean"},
                "promptsListChanged": {"type": "boolean"},
                "resourcesListChanged": {"type": "boolean"}
            }
        });
        let common_config = json!({
            "timeoutMs": {"type": "integer", "minimum": 1},
            "protocolVersion": {"type": "string", "enum": ["2026-07-28"]},
            "subscriptions": subscriptions
        });
        let mut stdio_properties = common_config.as_object().unwrap().clone();
        stdio_properties.insert("command".to_string(), json!({"type": "string"}));
        stdio_properties.insert(
            "args".to_string(),
            json!({"type": "array", "items": {"type": "string"}}),
        );
        stdio_properties.insert(
            "env".to_string(),
            json!({"type": "object", "additionalProperties": secret_ref}),
        );
        stdio_properties.insert("cwd".to_string(), json!({"type": "string"}));

        let mut http_properties = common_config.as_object().unwrap().clone();
        http_properties.insert("url".to_string(), json!({"type": "string"}));
        http_properties.insert(
            "headers".to_string(),
            json!({
                "type": "object",
                "additionalProperties": {
                    "oneOf": [
                        {"type": "string"},
                        {
                            "type": "object",
                            "properties": {"secretRef": {"type": "string"}},
                            "required": ["secretRef"],
                            "additionalProperties": false
                        }
                    ]
                }
            }),
        );

        let config = json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": stdio_properties,
                    "required": ["command"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": http_properties,
                    "required": ["url"],
                    "additionalProperties": false
                }
            ]
        });

        json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "method": {"type": "string", "enum": ["load"]},
                        "params": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "config": config
                            },
                            "required": ["name", "config"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["method", "params"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "method": {"type": "string", "enum": ["status"]},
                        "params": {
                            "oneOf": [
                                {
                                    "type": "object",
                                    "properties": {},
                                    "additionalProperties": false
                                },
                                {
                                    "type": "object",
                                    "properties": {"operationId": {"type": "string"}},
                                    "required": ["operationId"],
                                    "additionalProperties": false
                                },
                                {
                                    "type": "object",
                                    "properties": {"name": {"type": "string"}},
                                    "required": ["name"],
                                    "additionalProperties": false
                                }
                            ]
                        }
                    },
                    "required": ["method"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "method": {"type": "string", "enum": ["unload"]},
                        "params": {
                            "type": "object",
                            "properties": {"name": {"type": "string"}},
                            "required": ["name"],
                            "additionalProperties": false
                        }
                    },
                    "required": ["method", "params"],
                    "additionalProperties": false
                }
            ]
        })
    }

    fn bind_invocation(
        &self,
        input: Value,
    ) -> Result<Option<BoundToolInvocation>, Box<dyn std::error::Error + Send + Sync>> {
        let action = DynamicMcpAction::from_tool_input(input)?.canonicalize()?;
        let action = match action {
            CanonicalDynamicMcpAction::Unload(mut request) => {
                let status = self.deployment.capability(&self.session_id).snapshot();
                request.expected_instance = status
                    .servers
                    .get(&request.name)
                    .map(|server| server.instance_key.clone());
                CanonicalDynamicMcpAction::Unload(request)
            }
            action => action,
        };
        let method = action.method();
        Ok(Some(BoundToolInvocation {
            policy_name: method.policy_name().to_string(),
            policy_input: match &action {
                CanonicalDynamicMcpAction::Load(request) => json!({
                    "name": request.name,
                    "config": request.config.safe_summary()
                }),
                _ => action.policy_projection(),
            },
            target: Arc::new(BoundDynamicMcpTool {
                session_id: self.session_id.clone(),
                deployment: Arc::clone(&self.deployment),
                action,
                policy_name: method.policy_name(),
            }),
        }))
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err(Box::new(DynamicMcpToolError(
            "DynamicMCP must execute through canonical dispatch".to_string(),
        )))
    }
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
