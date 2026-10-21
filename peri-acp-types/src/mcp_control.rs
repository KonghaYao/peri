//! ACP session MCP control and the current pool catalog contract.
use crate::{
    plugin::{McpServerConfig, OAuthConfig},
    tools::BaseTool,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum McpControlRequest {
    Register {
        session_id: String,
        server_name: String,
        config: McpRegistrationConfig,
    },
    Restart {
        session_id: String,
        server_name: String,
    },
    Stop {
        session_id: String,
        server_name: String,
    },
}
impl McpControlRequest {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Register { session_id, .. }
            | Self::Restart { session_id, .. }
            | Self::Stop { session_id, .. } => session_id,
        }
    }
    pub fn server_name(&self) -> &str {
        match self {
            Self::Register { server_name, .. }
            | Self::Restart { server_name, .. }
            | Self::Stop { server_name, .. } => server_name,
        }
    }
    pub fn action(&self) -> McpControlAction {
        match self {
            Self::Register { .. } => McpControlAction::Register,
            Self::Restart { .. } => McpControlAction::Restart,
            Self::Stop { .. } => McpControlAction::Stop,
        }
    }
    pub fn validate(&self) -> Result<(), McpControlError> {
        if self.session_id().trim().is_empty()
            || self.server_name().is_empty()
            || self.server_name().len() > 256
            || self.server_name().chars().any(char::is_control)
        {
            return Err(McpControlError::invalid("invalid session or server name"));
        }
        if let Self::Register {
            server_name,
            config,
            ..
        } = self
        {
            if !server_name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            {
                return Err(McpControlError::invalid("invalid registration name"));
            }
            if crate::builtin_mcp::BUILTIN_MCP_INSTANCES
                .iter()
                .any(|item| item.instance.eq_ignore_ascii_case(server_name))
            {
                return Err(McpControlError::invalid("reserved MCP server name"));
            }
            config.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum McpRegistrationConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
        #[serde(default)]
        oauth: Option<OAuthConfig>,
    },
}
impl McpRegistrationConfig {
    pub fn validate(&self) -> Result<(), McpControlError> {
        match self {
            Self::Stdio { command, args, env } => {
                if command.trim().is_empty()
                    || command.contains('\0')
                    || args.iter().any(|v| v.contains('\0'))
                    || env
                        .iter()
                        .any(|(k, v)| k.is_empty() || k.contains(['\0', '=']) || v.contains('\0'))
                {
                    return Err(McpControlError::invalid("invalid stdio configuration"));
                }
            }
            Self::Http { url, headers, .. } => {
                let parsed = url::Url::parse(url)
                    .map_err(|_| McpControlError::invalid("invalid HTTP URL"))?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || parsed.host_str().is_none()
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.fragment().is_some()
                    || headers.iter().any(|(k, v)| {
                        k.is_empty()
                            || k.contains(['\r', '\n', '\0'])
                            || v.contains(['\r', '\n', '\0'])
                    })
                {
                    return Err(McpControlError::invalid("invalid HTTP configuration"));
                }
            }
        }
        Ok(())
    }
    pub fn into_server_config(self) -> McpServerConfig {
        let mut config = McpServerConfig {
            command: None,
            args: None,
            env: None,
            url: None,
            headers: None,
            oauth: None,
            disabled: None,
            subscriptions: None,
            system_mcp: None,
            system_mcp_tools: None,
            system_mcp_timeout: None,
            source: None,
        };
        match self {
            Self::Stdio { command, args, env } => {
                config.command = Some(command);
                config.args = Some(args);
                config.env = Some(env);
            }
            Self::Http {
                url,
                headers,
                oauth,
            } => {
                config.url = Some(url);
                config.headers = Some(headers);
                config.oauth = oauth;
            }
        }
        config
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpControlAction {
    Register,
    Restart,
    Stop,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpControlStatus {
    Connected,
    Stopped,
    NeedsAuthorization,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpControlResponse {
    pub session_id: String,
    pub server_name: String,
    pub action: McpControlAction,
    pub status: McpControlStatus,
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpControlError {
    pub code: i32,
    pub kind: String,
    pub message: String,
}
impl McpControlError {
    pub fn new(code: i32, kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            kind: kind.into(),
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(-32602, "invalid_request", message)
    }
    pub fn unsupported() -> Self {
        Self::new(-32000, "unsupported", "MCP server control is unavailable")
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerControl {
    pub restart: bool,
    pub stop: bool,
    pub unavailable_reason: Option<String>,
}
impl McpServerControl {
    pub fn available() -> Self {
        Self {
            restart: true,
            stop: true,
            unavailable_reason: None,
        }
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            restart: false,
            stop: false,
            unavailable_reason: Some(reason.into()),
        }
    }
}
impl Default for McpServerControl {
    fn default() -> Self {
        Self::unavailable("unavailable")
    }
}
#[derive(Clone, Default)]
pub struct SessionMcpCatalogSnapshot {
    pub generation: u64,
    pub tools: Vec<Arc<dyn BaseTool>>,
}
pub trait SessionMcpCatalogPort: Send + Sync {
    fn snapshot(&self) -> Arc<SessionMcpCatalogSnapshot>;
}

#[cfg(test)]
#[path = "mcp_control_test.rs"]
mod tests;
