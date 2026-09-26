use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// 3.0 批 2 波 1：协议类型归契约层（定义见 `peri_acp_types::plugin`）。
// `ConfigSource` / `McpServerConfig` / `OAuthConfig` 自本文件迁出；
// 本模块保留 re-export 保兼容。
pub use peri_acp_types::plugin::{ConfigSource, McpServerConfig, OAuthConfig};

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
    /// typed 配置不满足契约不变量（含手工构造的 `McpServerConfig`）。
    #[error("MCP 服务器配置无效: {server_name}: {source}")]
    InvalidServer {
        server_name: String,
        #[source]
        source: peri_acp_types::plugin::McpServerConfigValidationError,
    },
    /// 插件 MCP 配置加载失败（MCP 专用严格插件路径）。
    #[error("插件 MCP 配置加载失败: {source}")]
    PluginLoadError {
        #[source]
        source: crate::plugin::loader::LoaderError,
    },
    /// builtin 保留实例名被用户配置用 `command` / `url` 接管（A3）。
    ///
    /// 必须加载期拒绝：parity 与关闭语义都按名字反查，外部同名 server 一旦接管
    /// 该名字会继承「按原始名判定」的审批结果，静默移除 `mcp__*` 审批门。
    /// 错误文本只含实例名，不含路径 / env / 凭据。
    #[error("builtin 保留实例名不得被 command/url 接管: {name}")]
    ReservedBuiltinInstanceName { name: String },
    /// builtin 实例的关闭片段非法（A18）：唯一合法写法是只写 `disabled: true`。
    ///
    /// `disabled` 与 `system_mcp` 同时声明在今天会走到 readiness 的
    /// `Err(SystemReadinessError::Disabled)` fatal，阻断**所有** session，
    /// 因此必须在加载期拒绝。错误文本只含实例名。
    #[error("builtin 实例的关闭片段非法（disabled 与 system_mcp 不得同时声明）: {name}")]
    BuiltinClosureFragmentInvalid { name: String },
}

/// overlay 的加载期错误 → 配置错误（只搬运实例名，不拼任何路径 / env / 凭据）。
fn builtin_overlay_error(error: super::builtin::BuiltinOverlayError) -> McpConfigError {
    match error {
        super::builtin::BuiltinOverlayError::ReservedBuiltinInstanceName { name } => {
            McpConfigError::ReservedBuiltinInstanceName { name }
        }
        super::builtin::BuiltinOverlayError::DisabledWithSystemMcp { name } => {
            McpConfigError::BuiltinClosureFragmentInvalid { name }
        }
    }
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

/// 把一段无类型的 `mcpServers` JSON 解析为 typed 配置。
///
/// 非法组合在此处即失败（`McpServerConfig` 的 Deserialize 会跑契约校验），
/// 不再 `unwrap_or_default()` 退化成空配置——非法不是「无配置」。
fn parse_servers_value(
    value: &serde_json::Value,
    path: &Path,
) -> Result<HashMap<String, McpServerConfig>, McpConfigError> {
    serde_json::from_value::<HashMap<String, McpServerConfig>>(value.clone()).map_err(|source| {
        McpConfigError::ParseError {
            path: path.display().to_string(),
            source,
        }
    })
}

/// 校验一段 `mcpServers` JSON：解析失败或任一 server 不满足契约即 Err。
fn validate_servers_value(value: &serde_json::Value, path: &Path) -> Result<(), McpConfigError> {
    let servers = parse_servers_value(value, path)?;
    validate_config(&McpConfigFile {
        mcp_servers: servers,
    })
}

/// 校验 typed 配置的每个 server：按 server name 排序，首个错误稳定返回。
///
/// `disabled = true` 也照常校验——禁用不是绕过配置契约的通道。
pub(crate) fn validate_config(config: &McpConfigFile) -> Result<(), McpConfigError> {
    let mut names: Vec<&String> = config.mcp_servers.keys().collect();
    names.sort();
    for name in names {
        if let Some(cfg) = config.mcp_servers.get(name) {
            cfg.validate()
                .map_err(|source| McpConfigError::InvalidServer {
                    server_name: name.clone(),
                    source,
                })?;
        }
    }
    Ok(())
}

/// 从全局 settings.json 的 extra 字段中提取 mcpServers
///
/// 两个候选 map（`config.mcpServers` 与顶层 `mcpServers`）都存在时**两者都先校验**：
/// 写入口可能操作的是备用 map，非法备用 map 不能静默通过；选择仍按既有优先级
/// （nested > top-level）。
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
    let nested = v.get("config").and_then(|c| c.get("mcpServers"));
    let top_level = v.get("mcpServers");
    let nested_servers = match nested {
        Some(map) => Some(parse_servers_value(map, settings_json_path)?),
        None => None,
    };
    let top_level_servers = match top_level {
        Some(map) => Some(parse_servers_value(map, settings_json_path)?),
        None => None,
    };
    Ok(McpConfigFile {
        mcp_servers: nested_servers.or(top_level_servers).unwrap_or_default(),
    })
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
    if let Some(protocol_version) = &cfg.protocol_version {
        protocol_version.hash(&mut hasher);
    }
    // System 启动依赖字段参与 hash：变更它们必须视为不同服务器。
    if let Some(system_mcp) = &cfg.system_mcp {
        system_mcp.hash(&mut hasher);
    }
    if let Some(system_mcp_tools) = &cfg.system_mcp_tools {
        system_mcp_tools.hash(&mut hasher);
    }
    if let Some(system_mcp_timeout) = &cfg.system_mcp_timeout {
        system_mcp_timeout.hash(&mut hasher);
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
        protocol_version: config.protocol_version,
        source: config.source.clone(),
        subscriptions: config.subscriptions.clone(),
        // System key 原样复制：工具名数组是字面量，不得走 `expand`（否则 `${VAR}`
        // 形态的工具名会被替换），`Some([])` 与 `None` 必须保持可区分。
        system_mcp: config.system_mcp,
        system_mcp_tools: config.system_mcp_tools.clone(),
        system_mcp_timeout: config.system_mcp_timeout,
    }
}

/// 对 McpServerConfig 中所有字符串字段执行环境变量展开（无插件上下文）
pub(crate) fn expand_server_config(config: &McpServerConfig) -> McpServerConfig {
    expand_server_config_with_context(config, None, None, None)
}

/// 加载并合并 MCP 配置：全局 + 插件 + 项目级三层合并（生产入口）。
///
/// 全局路径由 `~/.peri/settings.json` 决定；任何一层非法都返回错误，
/// 不降级为空配置。
///
/// **builtin 注入策略的唯一 env 读取点**（IF-D3 / A1）：本函数读一次
/// `PERI_MCP_BUILTIN` 并把策略作为显式参数向下传；`_with_paths` 与
/// `apply_builtin_overlay` 都不读 env（否则「默认注入」与「测试路径」会分叉）。
pub(crate) fn load_merged_config_full(
    cwd: &Path,
    claude_home: &Path,
) -> Result<(McpConfigFile, HashMap<String, String>), McpConfigError> {
    let global_path = dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
        .join("settings.json");
    let policy = super::builtin::builtin_injection_policy_from_env();
    load_merged_config_full_with_paths(cwd, claude_home, &global_path, &policy)
}

/// 加载并合并 MCP 配置：全局 + 插件 + 项目级三层合并
/// 优先级：global < plugin < project（项目级最高）
/// 内容 hash 去重：手动配置（global/project）覆盖插件配置
/// 所有字段执行 ${VAR} 展开，插件来源在合并前即完成 per-plugin 独立上下文展开
/// 返回合并后的配置 + plugin_sources（marketplace 追踪，用于 UI 展示插件来源）
/// plugin_sources 的 key 格式为 `"plugin:{name}:{server}"`，
/// 与工具名 `mcp__{plugin_name}__{server_name}` 中的 server 部分一致
///
/// 内部实现：允许注入全局路径（测试 seam）。加载顺序与校验顺序一致——被选择加载的
/// global / plugin / project 输入先验证，再覆盖与去重；缺文件仍是空配置，非法文件不是。
/// 插件来源走 MCP 专用严格入口（`load_enabled_plugins_for_mcp`），宽容聚合 API
/// 不作为启动输入。
///
/// `policy` 是 builtin 默认层的**显式**注入策略（A1 / IF-D3）：本函数不读 env，
/// 调用方（`load_merged_config_full` 或测试）负责给出策略。
pub(crate) fn load_merged_config_full_with_paths(
    cwd: &Path,
    claude_home: &Path,
    global_path: &Path,
    policy: &super::builtin::BuiltinInjectionPolicy,
) -> Result<(McpConfigFile, HashMap<String, String>), McpConfigError> {
    let mut plugin_sources: HashMap<String, String> = HashMap::new();

    // 1. 加载全局配置（~/.peri/settings.json）
    let mut global = load_global_config(global_path)?;
    for cfg in global.mcp_servers.values_mut() {
        cfg.source = Some(ConfigSource::Global(global_path.to_path_buf()));
    }

    // 2. 加载插件 MCP 配置（claude_home 目录下的已启用插件）
    // 每插件独立上下文展开 env 变量，同时构建 plugin_sources（marketplace 追踪）
    let plugins = crate::plugin::loader::load_enabled_plugins_for_mcp(claude_home, None)
        .map_err(|source| McpConfigError::PluginLoadError { source })?;

    let mut plugin_servers: HashMap<String, McpServerConfig> = HashMap::new();
    for plugin in &plugins {
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
    let mut project = load_from_path(&project_path)?;
    for cfg in project.mcp_servers.values_mut() {
        cfg.source = Some(ConfigSource::Project(project_path.clone()));
    }

    // 4. 内容 hash 去重：移除与手动配置（global/project）内容相同的插件服务器
    // System MCP 不参与：其 namespace 归属必须保留，不得因跨 namespace 内容相同而消失。
    let manual_hashes: std::collections::HashSet<u64> = global
        .mcp_servers
        .values()
        .chain(project.mcp_servers.values())
        .map(server_config_hash)
        .collect();
    plugin_servers.retain(|_, cfg| {
        if cfg.system_mcp == Some(true) {
            return true;
        }
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

    // 6.5 builtin 默认配置层注入（A1 冻结的**唯一**注入点）：在变量展开之后、
    // step 7 校验之前，使 `run_initialize` 与公开 `load_merged_config` 看到同一份
    // 有效配置。位置在 step 4 的 hash 去重之后 ⇒ builtin 条目不进 `manual_hashes`，
    // 不改变既有 server 的去重结果。
    //
    // 规则 3（保留名接管）与规则 5（非法关闭片段）在**任何策略下**都生效：
    // `PERI_MCP_BUILTIN=off` 只抑制注入，不解除这两项加载期保护。
    super::builtin::apply_builtin_overlay(&mut merged.mcp_servers, policy)
        .map_err(builtin_overlay_error)?;

    // 7. 合并结果再次校验：覆盖与去重之后仍必须是合法配置。
    validate_config(&merged)?;

    Ok((merged, plugin_sources))
}

/// 加载并合并 MCP 配置（公开 API）。
///
/// 返回类型从 `McpConfigFile` 变为 `Result`：配置错误必须可传播，不再有
/// fail-open 的兼容壳（非法配置曾被合并成「成功的空配置」）。
pub fn load_merged_config(cwd: &Path, claude_home: &Path) -> Result<McpConfigFile, McpConfigError> {
    Ok(load_merged_config_full(cwd, claude_home)?.0)
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
///
/// 写入口语义：**修改前**校验全部相关 server map，**修改后**再次校验待写结果；
/// 任一步失败都不调用 `atomic_write_json`、不改动任何字节。删除非法条目也拒绝
/// ——非法配置需先由用户修复，删除不是修复通道。
pub(crate) fn remove_server_from_config_with_paths(
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
            validate_config(&config)?;
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

        // 全局支路只操作 Value：写盘前必须走一遍 typed 校验（两个候选 map 都查）。
        validate_value_servers(&value, global_path)?;

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
            validate_value_servers(&value, global_path)?;
            atomic_write_json(global_path, &value)?;
            return Ok(());
        }
    }

    // 未在任何配置中找到该 server，幂等返回
    Ok(())
}

/// 校验全局 settings.json 的 Value 中所有存在的 `mcpServers` map
/// （nested 与 top-level 都查：写入口可能操作备用 map）。
fn validate_value_servers(value: &serde_json::Value, path: &Path) -> Result<(), McpConfigError> {
    let nested = value.get("config").and_then(|c| c.get("mcpServers"));
    let top_level = value.get("mcpServers");
    for map in [nested, top_level].into_iter().flatten() {
        validate_servers_value(map, path)?;
    }
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
pub(crate) fn set_server_disabled_with_paths(
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

        if let Some(map) = value.get("mcpServers") {
            validate_servers_value(map, &project_path)?;
        }

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
            if let Some(map) = value.get("mcpServers") {
                validate_servers_value(map, &project_path)?;
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

        // 全局支路只操作 Value：写盘前必须走一遍 typed 校验（两个候选 map 都查），
        // 且 disabled=true 不能成为绕过配置契约的通道。
        validate_value_servers(&value, global_path)?;

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

        validate_value_servers(&value, global_path)?;
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
        protocol_version: None,
        subscriptions: None,
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
        source: None,
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
