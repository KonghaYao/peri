use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// MCP 服务器配置来源
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// 项目级配置（{cwd}/.mcp.json）
    Project(PathBuf),
    /// 全局配置（~/.peri/settings.json）
    Global(PathBuf),
    /// 插件配置
    Plugin,
}

/// 单个 MCP 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// stdio 传输的可执行命令（如 "npx"）
    pub command: Option<String>,
    /// stdio 传输的命令参数
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// 传递给子进程的环境变量
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    /// Streamable HTTP 传输的 URL
    pub url: Option<String>,
    /// HTTP 请求的自定义头
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// OAuth 2.0 配置
    #[serde(default)]
    pub oauth: Option<OAuthConfig>,
    /// 是否禁用（默认 false，不序列化默认值以保持配置简洁）
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: Option<bool>,
    /// 配置来源（运行时标记，不序列化）
    #[serde(skip)]
    pub source: Option<ConfigSource>,
}

fn is_false(v: &Option<bool>) -> bool {
    !v.unwrap_or(false)
}

/// MCP 服务器 OAuth 2.0 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OAuthConfig {
    /// 是否启用 OAuth（默认 true）
    #[serde(default)]
    pub enabled: Option<bool>,
    /// OAuth 客户端 ID
    #[serde(default)]
    pub client_id: Option<String>,
    /// OAuth 客户端密钥（支持 ${VAR} 环境变量展开）
    #[serde(default)]
    pub client_secret: Option<String>,
    /// OAuth 权限范围列表
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

impl OAuthConfig {
    /// 判断 OAuth 是否启用，默认 true
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// MCP 配置文件顶层结构（.mcp.json / settings.json 中的 mcpServers 片段）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigFile {
    #[serde(default)]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

/// MCP 配置加载错误
#[derive(Debug, Error)]
pub enum McpConfigError {
    #[error("MCP 配置文件解析失败: {path}: {source}")]
    ParseError {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("MCP 配置文件读取失败: {path}: {source}")]
    ReadError {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("MCP 配置文件写入失败: {path}: {source}")]
    WriteError {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// 从指定 JSON 文件加载 MCP 配置，文件不存在时返回空配置
pub(crate) fn load_from_path(path: &Path) -> Result<McpConfigFile, McpConfigError> {
    if !path.exists() {
        return Ok(McpConfigFile::default());
    }
    let content = std::fs::read_to_string(path).map_err(|e| McpConfigError::ReadError {
        path: path.display().to_string(),
        source: e,
    })?;
    serde_json::from_str::<McpConfigFile>(&content).map_err(|e| McpConfigError::ParseError {
        path: path.display().to_string(),
        source: e,
    })
}

/// 从全局 settings.json 的 extra 字段中提取 mcpServers
pub(crate) fn load_global_config(
    settings_json_path: &Path,
) -> Result<McpConfigFile, McpConfigError> {
    if !settings_json_path.exists() {
        return Ok(McpConfigFile::default());
    }
    let content =
        std::fs::read_to_string(settings_json_path).map_err(|e| McpConfigError::ReadError {
            path: settings_json_path.display().to_string(),
            source: e,
        })?;
    let v: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| McpConfigError::ParseError {
            path: settings_json_path.display().to_string(),
            source: e,
        })?;
    // 从顶层 value 中提取 "config"."mcpServers" 或 "mcpServers"
    let mcp_servers = v
        .get("config")
        .and_then(|c| c.get("mcpServers"))
        .or_else(|| v.get("mcpServers"))
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    let config = McpConfigFile {
        mcp_servers: serde_json::from_value(mcp_servers).unwrap_or_default(),
    };
    Ok(config)
}

/// 基于 command+args+env 计算服务器配置的内容 hash，用于去重
pub(crate) fn server_config_hash(cfg: &McpServerConfig) -> u64 {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };

    let mut hasher = DefaultHasher::new();
    if let Some(cmd) = &cfg.command {
        cmd.hash(&mut hasher);
    }
    if let Some(args) = &cfg.args {
        args.hash(&mut hasher);
    }
    if let Some(env) = &cfg.env {
        let mut sorted: Vec<_> = env.iter().collect();
        sorted.sort_by_key(|(k, _)| *k);
        for (k, v) in sorted {
            k.hash(&mut hasher);
            v.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// 展开 s 中所有变量占位符，支持插件上下文：
/// - ${CLAUDE_PLUGIN_ROOT}: 替换为 plugin_install_path
/// - ${CLAUDE_PLUGIN_DATA}: 替换为 plugin_data_path
/// - ${user_config.X}: 从 user_config HashMap 中查找
/// - ${VAR}: 系统环境变量（fallback）
pub(crate) fn expand_env_vars_with_context(
    s: &str,
    plugin_install_path: Option<&Path>,
    plugin_data_path: Option<&Path>,
    user_config: Option<&HashMap<String, String>>,
) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // 消耗 '{'
            let var_name: String = chars.by_ref().take_while(|&ch| ch != '}').collect();
            if chars.peek() == Some(&'}') {
                chars.next(); // 消耗 '}'
            }
            let value = if var_name == "CLAUDE_PLUGIN_ROOT" {
                plugin_install_path
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            } else if var_name == "CLAUDE_PLUGIN_DATA" {
                plugin_data_path
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            } else if let Some(key) = var_name.strip_prefix("user_config.") {
                user_config
                    .and_then(|uc| uc.get(key))
                    .cloned()
                    .unwrap_or_default()
            } else {
                match std::env::var(&var_name) {
                    Ok(val) => val,
                    Err(_) => {
                        tracing::warn!(
                            var_name = %var_name,
                            "MCP 配置环境变量 ${{{}}} 未设置，替换为空字符串",
                            var_name
                        );
                        String::new()
                    }
                }
            };
            result.push_str(&value);
        } else {
            result.push(c);
        }
    }
    result
}

/// 展开 s 中所有 ${VAR} 占位符为环境变量值（无插件上下文）
#[cfg(test)]
fn expand_env_vars(s: &str) -> String {
    expand_env_vars_with_context(s, None, None, None)
}

/// 对 McpServerConfig 中所有字符串字段执行环境变量展开（带插件上下文）
pub(crate) fn expand_server_config_with_context(
    config: &McpServerConfig,
    plugin_install_path: Option<&Path>,
    plugin_data_path: Option<&Path>,
    user_config: Option<&HashMap<String, String>>,
) -> McpServerConfig {
    let expand = |s: &str| -> String {
        expand_env_vars_with_context(s, plugin_install_path, plugin_data_path, user_config)
    };
    McpServerConfig {
        command: config.command.as_ref().map(|s| expand(s)),
        args: config
            .args
            .as_ref()
            .map(|arr| arr.iter().map(|s| expand(s)).collect()),
        env: config
            .env
            .as_ref()
            .map(|map| map.iter().map(|(k, v)| (k.clone(), expand(v))).collect()),
        url: config.url.as_ref().map(|s| expand(s)),
        headers: config
            .headers
            .as_ref()
            .map(|map| map.iter().map(|(k, v)| (k.clone(), expand(v))).collect()),
        oauth: config.oauth.as_ref().map(|o| OAuthConfig {
            enabled: o.enabled,
            client_id: o.client_id.clone(),
            client_secret: o.client_secret.as_ref().map(|s| expand(s)),
            scopes: o.scopes.clone(),
        }),
        disabled: config.disabled,
        source: config.source.clone(),
    }
}

/// 对 McpServerConfig 中所有字符串字段执行环境变量展开（无插件上下文）
pub(crate) fn expand_server_config(config: &McpServerConfig) -> McpServerConfig {
    expand_server_config_with_context(config, None, None, None)
}

/// 加载并合并 MCP 配置：全局 + 插件 + 项目级三层合并
/// 优先级：global < plugin < project（项目级最高）
/// 内容 hash 去重：手动配置（global/project）覆盖插件配置
/// 所有字段执行 ${VAR} 展开，插件来源在合并前即完成 per-plugin 独立上下文展开
/// 返回合并后的配置 + plugin_sources（marketplace 追踪，用于 UI 展示插件来源）
/// plugin_sources 的 key 格式为 `"plugin:{name}:{server}"`，
/// 与工具名 `mcp__{plugin_name}__{server_name}` 中的 server 部分一致
pub(crate) fn load_merged_config_full(
    cwd: &Path,
    claude_home: &Path,
) -> (McpConfigFile, HashMap<String, String>) {
    let mut plugin_sources: HashMap<String, String> = HashMap::new();

    // 1. 加载全局配置（~/.peri/settings.json）
    let global_path = dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
        .join("settings.json");
    let mut global = load_global_config(&global_path).unwrap_or_else(|e| {
        tracing::warn!(
            path = %global_path.display(),
            error = %e,
            "加载全局 MCP 配置失败，跳过"
        );
        McpConfigFile::default()
    });
    for cfg in global.mcp_servers.values_mut() {
        cfg.source = Some(ConfigSource::Global(global_path.clone()));
    }

    // 2. 加载插件 MCP 配置（~/.claude/ 目录下的已启用插件）
    // 每插件独立上下文展开 env 变量，同时构建 plugin_sources（marketing 追踪）
    let plugin_load_result =
        crate::plugin::loader::load_enabled_plugins_aggregated(claude_home, None);

    let mut plugin_servers: HashMap<String, McpServerConfig> = HashMap::new();
    for plugin in &plugin_load_result.plugins {
        for (name, config) in &plugin.mcp_servers {
            let namespaced = format!("plugin:{}:{}", plugin.name, name);
            let mut cfg = config.clone();
            cfg.source = Some(ConfigSource::Plugin);
            // 每插件独立上下文展开：在合并之前即完成 env 变量替换
            let mut expanded_cfg = expand_server_config_with_context(
                &cfg,
                Some(&plugin.install_path),
                Some(&plugin.data_path),
                None,
            );
            let env = expanded_cfg.env.get_or_insert_with(HashMap::new);
            env.insert(
                "CLAUDE_PLUGIN_ROOT".to_string(),
                plugin.install_path.to_string_lossy().to_string(),
            );
            env.insert(
                "CLAUDE_PLUGIN_DATA".to_string(),
                plugin.data_path.to_string_lossy().to_string(),
            );
            plugin_servers.insert(namespaced.clone(), expanded_cfg);

            // 构建 plugin_sources（key 与 config 中 server name 一致）
            // marketplace 现在直接来自 LoadedPlugin，无需额外加载 installed_plugins.json
            let source_id = format!(
                "{}@{}",
                plugin.name,
                if plugin.marketplace.is_empty() {
                    String::new()
                } else {
                    plugin.marketplace.clone()
                }
            );
            plugin_sources.insert(namespaced, source_id);
        }
    }

    // 3. 加载项目级配置（{cwd}/.mcp.json）
    let project_path = cwd.join(".mcp.json");
    let mut project = load_from_path(&project_path).unwrap_or_else(|e| {
        tracing::warn!(
            path = %project_path.display(),
            error = %e,
            "加载项目级 MCP 配置失败，跳过"
        );
        McpConfigFile::default()
    });
    for cfg in project.mcp_servers.values_mut() {
        cfg.source = Some(ConfigSource::Project(project_path.clone()));
    }

    // 4. 内容 hash 去重：移除与手动配置（global/project）内容相同的插件服务器
    let manual_hashes: std::collections::HashSet<u64> = global
        .mcp_servers
        .values()
        .chain(project.mcp_servers.values())
        .map(server_config_hash)
        .collect();
    plugin_servers.retain(|_, cfg| {
        let hash = server_config_hash(cfg);
        if manual_hashes.contains(&hash) {
            tracing::debug!("插件 MCP 服务器与手动配置内容相同（hash 去重），已跳过");
            false
        } else {
            true
        }
    });

    // 5. 三层合并：global → plugin → project
    let mut merged = global;
    for (name, cfg) in &plugin_servers {
        merged.mcp_servers.insert(name.clone(), cfg.clone());
    }
    for (name, server_config) in project.mcp_servers {
        merged.mcp_servers.insert(name, server_config);
    }

    // 6. 变量展开：插件来源已在 Step 2 完成 per-plugin 展开，此处跳过
    let names: Vec<String> = merged.mcp_servers.keys().cloned().collect();
    for name in names {
        if let Some(server_config) = merged.mcp_servers.get(&name).cloned() {
            let expanded = if matches!(server_config.source, Some(ConfigSource::Plugin)) {
                // 插件来源：已在 Step 2 完成上下文展开，直接使用
                server_config.clone()
            } else {
                expand_server_config(&server_config)
            };
            merged.mcp_servers.insert(name, expanded);
        }
    }

    (merged, plugin_sources)
}

/// 加载并合并 MCP 配置（公开 API，向后兼容）
/// 等同于 `load_merged_config_full().0`
pub fn load_merged_config(cwd: &Path, claude_home: &Path) -> McpConfigFile {
    load_merged_config_full(cwd, claude_home).0
}

/// 原子写入 JSON 文件（先写临时文件，再 rename 替换）
fn atomic_write_json(path: &Path, value: &serde_json::Value) -> Result<(), McpConfigError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp_path = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));

    let content = serde_json::to_string_pretty(value).map_err(|e| McpConfigError::WriteError {
        path: path.display().to_string(),
        source: e.into(),
    })?;

    use std::io::Write;
    let mut file = std::fs::File::create(&tmp_path).map_err(|e| McpConfigError::WriteError {
        path: path.display().to_string(),
        source: e,
    })?;
    file.write_all(content.as_bytes())
        .map_err(|e| McpConfigError::WriteError {
            path: path.display().to_string(),
            source: e,
        })?;
    drop(file);

    std::fs::rename(&tmp_path, path).map_err(|e| McpConfigError::WriteError {
        path: path.display().to_string(),
        source: e,
    })?;

    Ok(())
}

/// 从配置文件中删除指定的 MCP 服务器
/// 优先尝试项目级 .mcp.json，未找到则尝试全局 settings.json
pub fn remove_server_from_config(cwd: &Path, server_name: &str) -> Result<(), McpConfigError> {
    let global_path = dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
        .join("settings.json");
    remove_server_from_config_with_paths(cwd, &global_path, server_name)
}

/// 内部实现：允许注入全局路径（便于测试）
fn remove_server_from_config_with_paths(
    cwd: &Path,
    global_path: &Path,
    server_name: &str,
) -> Result<(), McpConfigError> {
    // 1. 尝试项目级删除
    let project_path = cwd.join(".mcp.json");
    if project_path.exists() {
        let content =
            std::fs::read_to_string(&project_path).map_err(|e| McpConfigError::ReadError {
                path: project_path.display().to_string(),
                source: e,
            })?;

        let mut config: McpConfigFile =
            serde_json::from_str(&content).map_err(|e| McpConfigError::ParseError {
                path: project_path.display().to_string(),
                source: e,
            })?;

        if config.mcp_servers.contains_key(server_name) {
            config.mcp_servers.remove(server_name);
            let value = serde_json::to_value(&config).map_err(|e| McpConfigError::WriteError {
                path: project_path.display().to_string(),
                source: e.into(),
            })?;
            atomic_write_json(&project_path, &value)?;
            return Ok(());
        }
    }

    // 2. 尝试全局删除
    if global_path.exists() {
        let content =
            std::fs::read_to_string(global_path).map_err(|e| McpConfigError::ReadError {
                path: global_path.display().to_string(),
                source: e,
            })?;

        let mut value: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| McpConfigError::ParseError {
                path: global_path.display().to_string(),
                source: e,
            })?;

        // 尝试 config.mcpServers 路径
        let mut removed = false;
        if let Some(config) = value
            .get_mut("config")
            .and_then(|c| c.get_mut("mcpServers"))
        {
            if let Some(servers) = config.as_object_mut() {
                if servers.remove(server_name).is_some() {
                    removed = true;
                }
            }
        }

        // 尝试顶层 mcpServers 路径
        if !removed {
            if let Some(servers) = value.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
                if servers.remove(server_name).is_some() {
                    removed = true;
                }
            }
        }

        if removed {
            atomic_write_json(global_path, &value)?;
            return Ok(());
        }
    }

    // 未在任何配置中找到该 server，幂等返回
    Ok(())
}

/// 在配置文件中设置指定 MCP 服务器的 disabled 状态
/// 优先尝试项目级 .mcp.json，未找到则尝试全局 settings.json
pub fn set_server_disabled(
    cwd: &Path,
    server_name: &str,
    disabled: bool,
) -> Result<(), McpConfigError> {
    let global_path = dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
        .join("settings.json");
    set_server_disabled_with_paths(cwd, &global_path, server_name, disabled)
}

/// 内部实现：允许注入全局路径（便于测试）
fn set_server_disabled_with_paths(
    cwd: &Path,
    global_path: &Path,
    server_name: &str,
    disabled: bool,
) -> Result<(), McpConfigError> {
    // 1. 尝试项目级
    let project_path = cwd.join(".mcp.json");
    if project_path.exists() {
        let content =
            std::fs::read_to_string(&project_path).map_err(|e| McpConfigError::ReadError {
                path: project_path.display().to_string(),
                source: e,
            })?;

        let mut value: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| McpConfigError::ParseError {
                path: project_path.display().to_string(),
                source: e,
            })?;

        if let Some(server_obj) = value
            .get_mut("mcpServers")
            .and_then(|s| s.get_mut(server_name))
            .and_then(|s| s.as_object_mut())
        {
            if disabled {
                server_obj.insert("disabled".to_string(), serde_json::Value::Bool(true));
            } else {
                server_obj.remove("disabled");
            }
            atomic_write_json(&project_path, &value)?;
            return Ok(());
        }
    }

    // 2. 尝试全局
    if global_path.exists() {
        let content =
            std::fs::read_to_string(global_path).map_err(|e| McpConfigError::ReadError {
                path: global_path.display().to_string(),
                source: e,
            })?;

        let mut value: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| McpConfigError::ParseError {
                path: global_path.display().to_string(),
                source: e,
            })?;

        // 尝试 config.mcpServers 路径
        let mut updated = false;
        if let Some(config) = value
            .get_mut("config")
            .and_then(|c| c.get_mut("mcpServers"))
        {
            if let Some(servers) = config.as_object_mut() {
                if let Some(server_val) = servers.get_mut(server_name) {
                    if let Some(obj) = server_val.as_object_mut() {
                        if disabled {
                            obj.insert("disabled".to_string(), serde_json::Value::Bool(true));
                        } else {
                            obj.remove("disabled");
                        }
                        updated = true;
                    }
                }
            }
        }

        // 尝试顶层 mcpServers 路径
        if !updated {
            if let Some(servers) = value.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
                if let Some(server_val) = servers.get_mut(server_name) {
                    if let Some(obj) = server_val.as_object_mut() {
                        if disabled {
                            obj.insert("disabled".to_string(), serde_json::Value::Bool(true));
                        } else {
                            obj.remove("disabled");
                        }
                    }
                }
            }
        }

        atomic_write_json(global_path, &value)?;
        return Ok(());
    }

    Ok(())
}

#[cfg(test)]
fn test_config() -> McpServerConfig {
    McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        source: None,
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
