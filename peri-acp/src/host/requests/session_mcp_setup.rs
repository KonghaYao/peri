//! ACP session setup MCP declarations consumed before the session pool starts.

use std::collections::HashMap;

use agent_client_protocol::schema::v1::McpServer;
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use serde_json::Value;

use crate::transport::types::AcpError;

pub(super) fn session_mcp_servers(
    params: &Value,
) -> Result<HashMap<String, McpServerConfig>, AcpError> {
    let Some(entries) = params.get("mcpServers") else {
        return Ok(HashMap::new());
    };
    let entries = entries
        .as_array()
        .ok_or_else(|| AcpError::new(-32602, "mcpServers must be an array"))?;
    let mut servers = HashMap::new();
    for entry in entries {
        let server: McpServer = serde_json::from_value(entry.clone())
            .map_err(|error| AcpError::new(-32602, format!("Invalid mcpServers entry: {error}")))?;
        let (name, mut config) = match server {
            McpServer::Http(http) => {
                if !http.url.starts_with("https://") && !http.url.starts_with("http://") {
                    return Err(AcpError::new(-32602, "HTTP MCP URL must use http or https"));
                }
                let headers: HashMap<_, _> = http
                    .headers
                    .into_iter()
                    .map(|h| (h.name, h.value))
                    .collect();
                let config: McpServerConfig = serde_json::from_value(
                    serde_json::json!({"url": http.url, "headers": headers}),
                )
                .map_err(|error| {
                    AcpError::new(-32602, format!("Invalid HTTP MCP server: {error}"))
                })?;
                (http.name, config)
            }
            McpServer::Stdio(stdio) => {
                let env: HashMap<_, _> = stdio.env.into_iter().map(|e| (e.name, e.value)).collect();
                let config: McpServerConfig = serde_json::from_value(
                    serde_json::json!({"command": stdio.command, "args": stdio.args, "env": env}),
                )
                .map_err(|error| {
                    AcpError::new(-32602, format!("Invalid stdio MCP server: {error}"))
                })?;
                (stdio.name, config)
            }
            McpServer::Acp(_) => continue,
            _ => return Err(AcpError::new(-32602, "Unsupported MCP transport")),
        };
        if name.is_empty()
            || (peri_acp_types::builtin_mcp::is_reserved_instance_name(&name)
                && name != "workspace")
            || (name == "workspace" && config.url.is_none())
        {
            return Err(AcpError::new(
                -32602,
                "Invalid MCP server name or workspace transport",
            ));
        }
        if name == "workspace" {
            config.source = Some(ConfigSource::WorkspaceRemote);
            config.system_mcp = Some(true);
            // A scope-signing key is host authority, never an ACP client's
            // extension field. SDK stdio injects the exact trusted endpoint.
            if let (Ok(url), Ok(secret)) = (
                std::env::var("PERI_TRUSTED_WORKSPACE_URL"),
                std::env::var("PERI_TRUSTED_WORKSPACE_SCOPE_SECRET_FILE"),
            ) {
                if config.url.as_deref() == Some(url.as_str()) {
                    config.task_scope_secret_file = Some(secret);
                }
            }
        }
        if servers.insert(name.clone(), config).is_some() {
            return Err(AcpError::new(
                -32602,
                format!("Duplicate MCP server: {name}"),
            ));
        }
    }
    Ok(servers)
}

#[cfg(test)]
mod acp_setup_tests {
    use super::*;

    #[test]
    fn http_workspace_setup_is_session_scoped_and_system_ready() {
        let servers = session_mcp_servers(&serde_json::json!({"mcpServers": [
            {"type":"http", "name":"workspace", "url":"https://example.test/mcp", "headers":[]}
        ]}))
        .unwrap();
        let workspace = &servers["workspace"];
        assert_eq!(workspace.url.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(workspace.source, Some(ConfigSource::WorkspaceRemote));
        assert_eq!(workspace.system_mcp, Some(true));
        assert!(workspace.system_mcp_tools.is_none());
    }

    #[test]
    fn invalid_session_server_list_is_rejected() {
        assert!(session_mcp_servers(&serde_json::json!({"mcpServers": {}})).is_err());
        assert!(
            session_mcp_servers(&serde_json::json!({"mcpServers": [
                {"type":"http", "name":"workspace", "url":"https://a.test", "headers":[]},
                {"type":"http", "name":"workspace", "url":"https://b.test", "headers":[]}
            ]}))
            .is_err()
        );
    }
}
