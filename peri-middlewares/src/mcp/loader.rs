//! Dynamic MCP loaders（plan 5.1–5.3 / B4）。
//!
//! 每个获准的 dynamic 配置投影为一条 deferred 宿主工具 `LoadMCP__<id>`。模型只拿到
//! 名称；传输地址、headers、env、cwd 与凭证都在调用时从池内受控配置解析。名称中的
//! 摘要覆盖配置作用域、插件身份与最终 server key，不使用可变显示名做绑定。
//!
//! Loader 通过 `mcp_server_name()` 声明受控来源，供目录归属与撤销使用；它没有远端
//! wire 工具名（`mcp_tool_name()` 为 `None`），因此不能冒充 MCP App 业务工具。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::mcp_control::SessionToolCatalogPreparation;
use peri_acp_types::mcp_control::SessionToolCatalogPreparationPort;
use peri_acp_types::mcp_control::SessionToolSource;
use peri_agent::tools::BaseTool;
use peri_agent::tools::ToolContext;
use serde_json::json;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use thiserror::Error;

use super::client::ClientStatus;
use super::client::McpClientPool;
use super::client::McpPoolError;
use super::config::ConfigSource;
use super::McpServerConfig;

pub(crate) const LOADER_PREFIX: &str = "LoadMCP__";
/// 名称长度上限 64：前缀 9 + 规范化键 32 + 分隔符 1 + 摘要 16。
const MAX_KEY_CHARS: usize = 32;
const DIGEST_BYTES: usize = 8;

/// 一条 Loader 与受控配置的绑定身份。
///
/// 只包含非秘密来源事实；凭证、URL、env 与 headers 不参与身份，也不进入名称。
#[derive(Clone, PartialEq, Eq)]
struct LoaderIdentity {
    server_key: String,
    scope: String,
    plugin: Option<String>,
    transport: &'static str,
}

impl LoaderIdentity {
    fn from_config(server_key: &str, config: &McpServerConfig, plugin: Option<&str>) -> Self {
        let scope = match &config.source {
            Some(ConfigSource::Project(path)) => format!("project:{}", path.display()),
            Some(ConfigSource::Global(path)) => format!("global:{}", path.display()),
            Some(ConfigSource::Plugin) => "plugin".to_string(),
            Some(ConfigSource::WorkspaceRemote) => "workspace_remote".to_string(),
            Some(ConfigSource::Acp) => "acp".to_string(),
            Some(ConfigSource::Builtin { instance }) => format!("builtin:{instance}"),
            None => "unspecified".to_string(),
        };
        let transport = if config.command.is_some() {
            "stdio"
        } else if config.url.is_some() {
            "http"
        } else {
            "unconfigured"
        };
        Self {
            server_key: server_key.to_owned(),
            scope,
            plugin: plugin.map(str::to_owned),
            transport,
        }
    }

    /// 长度定界的 SHA-256 截断：跨运行稳定，且不能从名称反推配置内容。
    fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"peri-dynamic-mcp-loader-v1");
        for (tag, value) in [
            ("server", Some(self.server_key.as_str())),
            ("scope", Some(self.scope.as_str())),
            ("plugin", self.plugin.as_deref()),
            ("transport", Some(self.transport)),
        ] {
            hasher.update(tag.as_bytes());
            hasher.update([0u8]);
            match value {
                Some(value) => {
                    hasher.update((value.len() as u64).to_be_bytes());
                    hasher.update(value.as_bytes());
                }
                None => hasher.update([0xff; 8]),
            }
        }
        hasher.finalize()[..DIGEST_BYTES]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn tool_name(&self) -> String {
        format!(
            "{LOADER_PREFIX}{}_{}",
            normalize_key(&self.server_key),
            self.digest()
        )
    }

    /// 大小写折叠比较用的身份标签（不含凭证，仅用于冲突诊断）。
    fn tag(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.server_key,
            self.scope,
            self.plugin.as_deref().unwrap_or(""),
            self.transport
        )
    }
}

/// 规范化配置键：小写 ASCII 字母数字 + 单下划线，去首尾分隔并限长。
fn normalize_key(server_key: &str) -> String {
    let mut normalized = String::new();
    for ch in server_key.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
        } else if !normalized.is_empty() && !normalized.ends_with('_') {
            normalized.push('_');
        }
        if normalized.len() >= MAX_KEY_CHARS {
            break;
        }
    }
    let trimmed = normalized.trim_matches('_');
    if trimmed.is_empty() {
        "server".to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Error)]
enum LoaderError {
    #[error("MCP loader \"{loader}\" takes no arguments; unexpected field \"{field}\"")]
    UnexpectedArgument { loader: String, field: String },
    #[error("MCP loader \"{loader}\" takes no arguments")]
    InvalidArguments { loader: String },
    #[error("MCP loader \"{loader}\" is stale: its dynamic MCP source changed or was closed")]
    Stale { loader: String },
    #[error(
        "MCP loader \"{loader}\" cannot verify the next request catalog: this execution has no session tool catalog"
    )]
    CatalogUnavailable { loader: String },
    #[error(
        "MCP loader \"{loader}\" loaded MCP server \"{server}\" but the session catalog could not be prepared: {reason}"
    )]
    CatalogPrepare {
        loader: String,
        server: String,
        reason: String,
    },
}

/// 绑定一条 dynamic 配置的 deferred Loader。
pub(crate) struct DynamicToolLoader {
    pool: Arc<McpClientPool>,
    identity: LoaderIdentity,
    name: String,
    description: String,
    config_revision: u64,
}

impl DynamicToolLoader {
    fn new(
        pool: &Arc<McpClientPool>,
        server_key: &str,
        config: &McpServerConfig,
        plugin: Option<String>,
        config_revision: u64,
    ) -> Self {
        let identity = LoaderIdentity::from_config(server_key, config, plugin.as_deref());
        let name = identity.tool_name();
        let origin = plugin
            .as_deref()
            .map(|plugin| format!(" (plugin {plugin})"))
            .unwrap_or_default();
        let description = format!(
            "On-demand loader for the dynamic MCP server {server_key}{origin}. \
             Call it once to connect the server and make its tools available; it takes no \
             arguments. Transport address, headers, environment and credentials come from \
             trusted configuration."
        );
        Self {
            pool: pool.clone(),
            identity,
            name,
            description,
            config_revision,
        }
    }

    /// 只接受无参调用；任何字段都可能被用来改写目标，必须拒绝。
    fn validate_arguments(&self, input: &Value) -> Result<(), LoaderError> {
        match input {
            // 无参调用可能以 null 到达（宿主代理未传 arguments）；它不携带字段。
            Value::Null => Ok(()),
            Value::Object(map) if map.is_empty() => Ok(()),
            Value::Object(map) => Err(LoaderError::UnexpectedArgument {
                loader: self.name.clone(),
                field: map.keys().next().cloned().unwrap_or_default(),
            }),
            _ => Err(LoaderError::InvalidArguments {
                loader: self.name.clone(),
            }),
        }
    }

    /// 调用前重新核对绑定：配置仍存在、仍是 dynamic、版本与来源身份未变、来源未关闭。
    fn check_binding(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let server = &self.identity.server_key;
        if !self.pool.is_open() {
            return Err(Box::new(not_connected(server)));
        }
        let slot = self.pool.existing_server_slot(server);
        if slot
            .as_ref()
            .is_some_and(|slot| slot.stopped.load(Ordering::Acquire))
        {
            return Err(Box::new(not_connected(server)));
        }
        let Some(config) = self.pool.configs.read().get(server).cloned() else {
            return Err(Box::new(not_connected(server)));
        };
        if !config.dynamic || config.disabled.unwrap_or(false) {
            return Err(Box::new(not_connected(server)));
        }
        let revision = slot.as_ref().map_or(0, |slot| slot.config_revision());
        if revision != self.config_revision {
            return Err(Box::new(LoaderError::Stale {
                loader: self.name.clone(),
            }));
        }
        let plugin = self.pool.plugin_source_of(server);
        let current = LoaderIdentity::from_config(server, &config, plugin.as_deref());
        if current != self.identity {
            return Err(Box::new(LoaderError::Stale {
                loader: self.name.clone(),
            }));
        }
        Ok(())
    }

    /// 构造本 Loader 时绑定的插件来源；与名称摘要使用同一个身份快照。
    fn plugin_source(&self) -> Option<&str> {
        self.identity.plugin.as_deref()
    }

    /// 连接提交后验证当前会话的下一轮目录（plan 7 / 7.1）。
    ///
    /// 只读：不发布候选、不改变当前 Act 的工具视图；端口在执行期由 canonical
    /// dispatch 投影，Loader 不持久持有它。
    fn prepare_session_catalog(
        &self,
        port: Option<&Arc<dyn SessionToolCatalogPreparationPort>>,
        server: &str,
    ) -> Result<SessionToolCatalogPreparation, LoaderError> {
        let Some(port) = port else {
            return Err(LoaderError::CatalogUnavailable {
                loader: self.name.clone(),
            });
        };
        port.prepare_next()
            .map_err(|error| LoaderError::CatalogPrepare {
                loader: self.name.clone(),
                server: server.to_owned(),
                reason: error.to_string(),
            })
    }
}

#[async_trait]
impl BaseTool for DynamicToolLoader {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    /// 受控配置来源：供 MCP owner 过滤与撤销；Loader 没有远端 wire 工具名。
    fn mcp_server_name(&self) -> Option<&str> {
        Some(&self.identity.server_key)
    }

    async fn invoke(
        &self,
        input: Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.validate_arguments(&input)?;
        self.check_binding()?;
        let server = &self.identity.server_key;
        let reused = self
            .pool
            .client(server)
            .is_some_and(|handle| handle.status == ClientStatus::Connected);
        self.pool.ensure_loaded(server).await?;
        // 加载期间配置可能被替换或来源被关闭：只按当前绑定报告 ready。
        self.check_binding()?;
        let Some(handle) = self.pool.client(server) else {
            return Err(Box::new(not_connected(server)));
        };
        if handle.status != ClientStatus::Connected {
            return Err(Box::new(McpPoolError::NotConnected {
                server: server.clone(),
                status: handle.status.clone(),
            }));
        }
        // 目录验证是“可用”的一部分：无法构造下一轮目录时不得报告 ready。
        let preparation =
            self.prepare_session_catalog(ctx.session_tool_catalog.as_ref(), server)?;
        let session_tool_count = preparation
            .admitted_for(&SessionToolSource::Mcp(server.clone()))
            .iter()
            .filter(|name| name.as_str() != self.name)
            .count();
        let mut result = json!({
            "status": "ready",
            "server": server,
            "reused": reused,
            "catalog_generation": self.pool.catalog_generation.load(Ordering::Acquire),
            "available_tool_count": handle.tools.len(),
            "session_catalog_generation": preparation.generation,
            "session_tool_count": session_tool_count,
        });
        // 服务器有工具但本会话一个都不能调用时必须明说，不能暗示业务能力可用。
        if !handle.tools.is_empty() && session_tool_count == 0 {
            result["note"] = json!(format!(
                "MCP server \"{server}\" exposes {} tool(s), but none are callable in this \
                 session under its current tool policy; do not assume its business \
                 capabilities are available.",
                handle.tools.len()
            ));
        }
        Ok(result.to_string())
    }
}

/// 当前池内所有可投影的 dynamic Loader（供 `PoolCatalog` 合并）。
///
/// 关闭 / 禁用的来源不产出 Loader；投影按配置键排序，名称按大小写折叠去重。
pub(crate) fn dynamic_loader_tools(
    pool: &Arc<McpClientPool>,
    session: Option<&str>,
) -> Vec<Arc<DynamicToolLoader>> {
    if !pool.is_open() {
        return Vec::new();
    }
    let mut candidates: Vec<(String, McpServerConfig)> = pool
        .configs
        .read()
        .iter()
        .filter(|(_, config)| {
            config.dynamic && !config.disabled.unwrap_or(false) && config.system_mcp != Some(true)
        })
        .map(|(name, config)| (name.clone(), config.clone()))
        .collect();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));

    let mut seen: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut loaders: Vec<Arc<DynamicToolLoader>> = Vec::new();
    for (server_key, config) in candidates {
        if session.is_some_and(|session| !pool.is_visible_to_session(&server_key, session)) {
            continue;
        }
        let slot = pool.existing_server_slot(&server_key);
        if slot
            .as_ref()
            .is_some_and(|slot| slot.stopped.load(Ordering::Acquire))
        {
            continue;
        }
        let revision = slot.as_ref().map_or(0, |slot| slot.config_revision());
        let plugin = pool.plugin_source_of(&server_key);
        let loader = DynamicToolLoader::new(pool, &server_key, &config, plugin, revision);
        let folded = loader.name.to_ascii_lowercase();
        if let Some(existing) = seen.get(&folded) {
            if existing != &loader.identity.tag() {
                tracing::warn!(
                    loader = %loader.name,
                    server = %server_key,
                    existing = %existing,
                    "dynamic MCP loader name collides after case folding; keeping the first"
                );
                continue;
            }
        }
        seen.insert(folded, loader.identity.tag());
        loaders.push(Arc::new(loader));
    }
    loaders
}

/// 当前池内可投影的 dynamic Loader 绑定（Host 生成插件指引的受控投影）。
///
/// 与 [`dynamic_loader_tools`] 走同一条可见性 / 关闭 / 去重 / 排序规则；
/// 名称与插件归属必须取自同一个 `LoaderIdentity`，不能在两次读取之间
/// 让共享 `plugin_sources` 的替换拆散它们。
pub(crate) fn dynamic_loader_bindings(
    pool: &Arc<McpClientPool>,
    session: Option<&str>,
) -> Vec<peri_acp_types::mcp_control::DynamicLoaderBinding> {
    bindings_from(&dynamic_loader_tools(pool, session))
}

fn bindings_from(
    loaders: &[Arc<DynamicToolLoader>],
) -> Vec<peri_acp_types::mcp_control::DynamicLoaderBinding> {
    loaders
        .iter()
        .map(|loader| peri_acp_types::mcp_control::DynamicLoaderBinding {
            loader: loader.name.clone(),
            server_key: loader.identity.server_key.clone(),
            plugin_source: loader.plugin_source().map(str::to_owned),
        })
        .collect()
}

fn not_connected(server: &str) -> McpPoolError {
    McpPoolError::NotConnected {
        server: server.to_owned(),
        status: ClientStatus::Disconnected,
    }
}

#[cfg(test)]
#[path = "loader_test.rs"]
mod tests;
