use std::collections::HashMap;

use peri_acp_types::plugin::{ConfigSource, McpServerConfigValidationError};
use thiserror::Error;

use super::config::McpServerConfig;

/// 传输形态三分类（IF-D1）。
///
/// 连接超时选择与失败日志的 `transport` 字段都从它派生，**禁止**再用
/// `matches!(…, StreamableHttp { .. })` 这类二元判定（那会把 builtin 误判成 stdio：
/// 既走错超时，也把日志字段写成事实错误）。
#[allow(dead_code)] // 三分类接线归 E-03（W2）；此处先落地接口并有 crate 内测试覆盖
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportKind {
    Stdio,
    Http,
    Builtin,
}

/// builtin 实例的连接握手超时：同进程握手 + 一次 `tools/list` 的实测上界远小于它。
#[allow(dead_code)] // 三分类接线归 E-03（W2）；此处先落地接口并有 crate 内测试覆盖
pub(crate) const BUILTIN_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    /// 同进程 builtin 实例（`instance` 必须能在 builtin 注册表中解析）。
    Builtin { instance: String },
}

impl TransportConfig {
    /// 三分类：超时选择与失败日志共用同一结果。
    #[allow(dead_code)] // 三分类接线归 E-03（W2）；此处先落地接口并有 crate 内测试覆盖
    pub(crate) fn kind(&self) -> TransportKind {
        match self {
            TransportConfig::Stdio { .. } => TransportKind::Stdio,
            TransportConfig::StreamableHttp { .. } => TransportKind::Http,
            TransportConfig::Builtin { .. } => TransportKind::Builtin,
        }
    }
}

/// 传输层构建错误
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("MCP 服务器配置无效: 缺少 command 或 url 字段")]
    InvalidConfig,
    /// builtin 身份无法在注册表中解析（错误文本只含实例名，不含路径 / env / 凭据）。
    #[error("builtin MCP 实例未注册: {instance}")]
    UnknownBuiltinInstance { instance: String },
    /// typed 配置不满足契约不变量。`McpServerConfig` 是公开 struct，可手工构造，
    /// 因此 Deserialize 不是唯一闸门——建传输前同样要过同一份纯校验。
    #[error(transparent)]
    InvalidSystemConfig(#[from] McpServerConfigValidationError),
}

/// builtin 实例身份校验（消费侧在 spawn 前调用；`TryFrom` 自身不查注册表）。
///
/// 未注册的实例名不得建立传输、不得静默降级成其它形态。
pub(crate) fn require_known_builtin_instance(instance: &str) -> Result<(), TransportError> {
    if peri_acp_types::builtin_mcp::find(instance).is_some() {
        Ok(())
    } else {
        Err(TransportError::UnknownBuiltinInstance {
            instance: instance.to_string(),
        })
    }
}

impl TryFrom<&McpServerConfig> for TransportConfig {
    type Error = TransportError;

    fn try_from(config: &McpServerConfig) -> Result<Self, Self::Error> {
        // System key 组合非法（含显式 `[]` 无 `system_mcp = true`）不得建立传输。
        config.validate()?;
        // builtin 身份唯一来源是运行时标记 `source`（`#[serde(skip)]`，用户配置无法伪造）。
        // 该判定**优先于** command / url：只有代码能构造 `ConfigSource::Builtin`。
        if let Some(ConfigSource::Builtin { instance }) = &config.source {
            return Ok(TransportConfig::Builtin {
                instance: instance.clone(),
            });
        }
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
