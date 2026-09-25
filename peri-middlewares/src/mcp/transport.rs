use std::collections::HashMap;

use peri_acp_types::plugin::McpServerConfigValidationError;
use thiserror::Error;

use super::config::McpServerConfig;

/// 传输层配置枚举，从 McpServerConfig 派生
#[derive(Debug, Clone)]
pub enum TransportConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    StreamableHttp {
        url: String,
        headers: HashMap<String, String>,
        /// OAuth 配置（仅当服务器配置了 oauth 且 is_enabled() 时为 Some）
        oauth: Option<super::config::OAuthConfig>,
    },
}

/// 传输层构建错误
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("MCP 服务器配置无效: 缺少 command 或 url 字段")]
    InvalidConfig,
    /// typed 配置不满足契约不变量。`McpServerConfig` 是公开 struct，可手工构造，
    /// 因此 Deserialize 不是唯一闸门——建传输前同样要过同一份纯校验。
    #[error(transparent)]
    InvalidSystemConfig(#[from] McpServerConfigValidationError),
}

impl TryFrom<&McpServerConfig> for TransportConfig {
    type Error = TransportError;

    fn try_from(config: &McpServerConfig) -> Result<Self, Self::Error> {
        // System key 组合非法（含显式 `[]` 无 `system_mcp = true`）不得建立传输。
        config.validate()?;
        match (&config.command, &config.url) {
            (Some(command), _) => Ok(TransportConfig::Stdio {
                command: command.clone(),
                args: config.args.clone().unwrap_or_default(),
                env: config.env.clone().unwrap_or_default(),
            }),
            (_, Some(url)) => Ok(TransportConfig::StreamableHttp {
                url: url.clone(),
                headers: config.headers.clone().unwrap_or_default(),
                oauth: config.oauth.as_ref().filter(|o| o.is_enabled()).cloned(),
            }),
            (None, None) => Err(TransportError::InvalidConfig),
        }
    }
}

#[cfg(test)]
#[path = "transport_test.rs"]
mod tests;
