use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use thiserror::Error;

#[cfg(test)]
mod cache_policy;

pub use peri_config::mcp::{McpCachePolicy, McpConfigFile, MCP_CACHE_ENV};

// 3.0 批 2 波 1：协议类型归契约层（定义见 `peri_acp_types::plugin`）。
// `ConfigSource` / `McpServerConfig` / `OAuthConfig` 自本文件迁出；
// 本模块保留 re-export 保兼容。
pub use peri_acp_types::plugin::{ConfigSource, McpServerConfig, OAuthConfig};

/// MCP 配置加载错误
#[derive(Debug, Error)]
pub enum McpConfigError {
    #[error(transparent)]
    Authority(#[from] peri_config::ConfigurationError),
    #[error("MCP snapshot scope mismatch: requested {cwd}, snapshot {snapshot_cwd}")]
    SnapshotScopeMismatch { cwd: String, snapshot_cwd: String },
    #[error("PERI_MCP_CACHE must be true/false, 1/0, or on/off")]
    InvalidCacheEnvironment,
    #[error("MCP cache environment configuration unavailable: {source}")]
    CacheEnvironmentRead { source: std::io::Error },
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

fn map_core_error(error: peri_config::mcp::McpConfigError, path: &Path) -> McpConfigError {
    match error {
        peri_config::mcp::McpConfigError::InvalidConfig(source) => McpConfigError::ParseError {
            path: path.display().to_string(),
            source,
        },
        peri_config::mcp::McpConfigError::InvalidServer {
            server_name,
            source,
        } => McpConfigError::InvalidServer {
            server_name,
            source,
        },
        peri_config::mcp::McpConfigError::InvalidCacheEnvironment => {
            McpConfigError::InvalidCacheEnvironment
        }
    }
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
    if !config_exists(path)? {
        return Ok(McpConfigFile::default());
    }
    let value = read_json_value(path)?.unwrap_or_default();
    peri_config::mcp::parse_project(&value).map_err(|error| map_core_error(error, path))
}

fn config_exists(path: &Path) -> Result<bool, McpConfigError> {
    peri_mcp_config::exists(path).map_err(|source| McpConfigError::ReadError {
        path: path.display().to_string(),
        source,
    })
}

fn read_json_value(path: &Path) -> Result<Option<serde_json::Value>, McpConfigError> {
    if !config_exists(path)? {
        return Ok(None);
    }
    let content = peri_mcp_config::read_text(path).map_err(|source| McpConfigError::ReadError {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|source| McpConfigError::ParseError {
            path: path.display().to_string(),
            source,
        })
}

/// 校验 typed 配置的每个 server：按 server name 排序，首个错误稳定返回。
///
/// `disabled = true` 也照常校验——禁用不是绕过配置契约的通道。
pub(crate) fn validate_config(config: &McpConfigFile) -> Result<(), McpConfigError> {
    peri_config::mcp::validate_config(config)
        .map_err(|error| map_core_error(error, Path::new("<typed MCP configuration>")))
}

/// 从全局 settings.json 的 extra 字段中提取 mcpServers
///
/// 两个候选 map（`config.mcpServers` 与顶层 `mcpServers`）都存在时**两者都先校验**：
/// 写入口可能操作的是备用 map，非法备用 map 不能静默通过；选择仍按既有优先级
/// （nested > top-level）。
pub(crate) fn load_global_config(
    settings_json_path: &Path,
) -> Result<McpConfigFile, McpConfigError> {
    if !config_exists(settings_json_path)? {
        return Ok(McpConfigFile::default());
    }
    let value = read_json_value(settings_json_path)?.unwrap_or_default();
    peri_config::mcp::parse_global(&value)
        .map_err(|error| map_core_error(error, settings_json_path))
}

/// 基于 command+args+env 计算服务器配置的内容 hash，用于去重
#[cfg(test)]
pub(crate) fn server_config_hash(cfg: &McpServerConfig) -> u64 {
    peri_config::mcp::server_config_hash(cfg)
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
/// **builtin 注入策略的配置加载边界**（IF-D3 / A1）：本函数读一次
/// `PERI_MCP_BUILTIN` 并把策略作为显式参数向下传；`_with_paths` 与
/// `apply_builtin_overlay` 都不读 env（否则「默认注入」与「测试路径」会分叉）。
pub(crate) fn load_merged_config_full(
    cwd: &Path,
    claude_home: &Path,
) -> Result<(McpConfigFile, HashMap<String, String>), McpConfigError> {
    let global_path = peri_mcp_config::global_config_path();
    let policy = super::builtin::builtin_injection_policy_from_env();
    let environment = cache_environment_input()?;
    load_merged_config_with_environment(cwd, claude_home, &global_path, &policy, &environment)
}

/// Bare 保留本地文件/终端能力，不读取用户、插件或项目 MCP 配置。
/// 显式运维关闭仍生效；工具声明和校验复用普通配置路径的同一实现。
pub(crate) fn load_bare_config() -> Result<McpConfigFile, McpConfigError> {
    let policy = super::builtin::builtin_injection_policy_from_env();
    let environment = cache_environment_input()?;
    let mut config = peri_config::mcp::resolve_from_files(
        &McpConfigFile::default(),
        &McpConfigFile::default(),
        &HashMap::new(),
        &environment,
    )
    .map_err(|error| map_core_error(error, Path::new("<environment>")))?;
    super::builtin::apply_builtin_overlay(&mut config.mcp_servers, &policy)
        .map_err(builtin_overlay_error)?;
    config.mcp_servers.retain(|name, _| name == "workspace");
    validate_config(&config)?;
    Ok(config)
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
#[cfg(test)]
pub(crate) fn load_merged_config_full_with_paths(
    cwd: &Path,
    claude_home: &Path,
    global_path: &Path,
    policy: &super::builtin::BuiltinInjectionPolicy,
) -> Result<(McpConfigFile, HashMap<String, String>), McpConfigError> {
    load_merged_config_with_environment(cwd, claude_home, global_path, policy, &BTreeMap::new())
}

fn cache_environment_input() -> Result<BTreeMap<String, String>, McpConfigError> {
    peri_mcp_config::read_environment(MCP_CACHE_ENV)
        .map_err(|source| McpConfigError::CacheEnvironmentRead { source })
        .map(|value| {
            value
                .map(|value| BTreeMap::from([(MCP_CACHE_ENV.to_string(), value)]))
                .unwrap_or_default()
        })
}

fn load_merged_config_with_environment(
    cwd: &Path,
    claude_home: &Path,
    global_path: &Path,
    policy: &super::builtin::BuiltinInjectionPolicy,
    environment: &BTreeMap<String, String>,
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
    let plugin_servers = collect_plugin_mcp_servers(&plugins, &mut plugin_sources);

    // 3. 加载项目级配置（{cwd}/.mcp.json）
    let project_path = cwd.join(".mcp.json");
    let mut project = load_from_path(&project_path)?;
    for cfg in project.mcp_servers.values_mut() {
        cfg.source = Some(ConfigSource::Project(project_path.clone()));
    }

    // 4. 核心负责三层来源优先级、内容 hash 去重和 cache policy。
    let merged =
        peri_config::mcp::resolve_from_files(&global, &project, &plugin_servers, environment)
            .map_err(|error| map_core_error(error, global_path))?;

    finalize_merged_config(merged, policy).map(|merged| (merged, plugin_sources))
}

pub(crate) fn load_merged_config_from_snapshot(
    cwd: &Path,
    claude_home: &Path,
    snapshot: &peri_config::ConfigurationSnapshot,
) -> Result<(McpConfigFile, HashMap<String, String>), McpConfigError> {
    let snapshot_cwd = &snapshot.scope().cwd;
    if snapshot_cwd != cwd {
        return Err(McpConfigError::SnapshotScopeMismatch {
            cwd: cwd.display().to_string(),
            snapshot_cwd: snapshot_cwd.display().to_string(),
        });
    }

    let mut plugin_sources = HashMap::new();
    let plugins = crate::plugin::loader::load_enabled_plugins_for_mcp(claude_home, None)
        .map_err(|source| McpConfigError::PluginLoadError { source })?;
    let plugin_servers = collect_plugin_mcp_servers(&plugins, &mut plugin_sources);
    let merged = snapshot.mcp_with_plugins(&plugin_servers)?;
    let policy = if snapshot.builtin_mcp_enabled() {
        super::builtin::BuiltinInjectionPolicy::all()
    } else {
        super::builtin::BuiltinInjectionPolicy::none()
    };
    let merged = finalize_merged_config(merged, &policy)?;
    Ok((merged, plugin_sources))
}

pub(crate) fn load_bare_config_from_snapshot(
    snapshot: &peri_config::ConfigurationSnapshot,
) -> Result<McpConfigFile, McpConfigError> {
    let mut config = snapshot.bare_mcp()?;
    let policy = if snapshot.builtin_mcp_enabled() {
        super::builtin::BuiltinInjectionPolicy::all()
    } else {
        super::builtin::BuiltinInjectionPolicy::none()
    };
    super::builtin::apply_builtin_overlay(&mut config.mcp_servers, &policy)
        .map_err(builtin_overlay_error)?;
    config.mcp_servers.retain(|name, _| name == "workspace");
    validate_config(&config)?;
    Ok(config)
}

fn collect_plugin_mcp_servers(
    plugins: &[crate::plugin::loader::LoadedPlugin],
    plugin_sources: &mut HashMap<String, String>,
) -> HashMap<String, McpServerConfig> {
    let mut plugin_servers = HashMap::new();
    for plugin in plugins {
        for (name, config) in &plugin.mcp_servers {
            let namespaced = peri_config::mcp::plugin_server_name(&plugin.name, name);
            let mut config = config.clone();
            config.source = Some(ConfigSource::Plugin);
            let mut expanded = expand_server_config_with_context(
                &config,
                Some(&plugin.install_path),
                Some(&plugin.data_path),
                None,
            );
            let environment = expanded.env.get_or_insert_with(HashMap::new);
            environment.insert(
                "CLAUDE_PLUGIN_ROOT".to_string(),
                plugin.install_path.to_string_lossy().to_string(),
            );
            environment.insert(
                "CLAUDE_PLUGIN_DATA".to_string(),
                plugin.data_path.to_string_lossy().to_string(),
            );
            plugin_servers.insert(namespaced.clone(), expanded);
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
    plugin_servers
}

fn finalize_merged_config(
    mut merged: McpConfigFile,
    policy: &super::builtin::BuiltinInjectionPolicy,
) -> Result<McpConfigFile, McpConfigError> {
    // Plugin config is already expanded with its own context; other sources use process env.
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

    // 6.5 普通配置路径的 builtin 默认层注入（bare 另选 workspace 子集）：在变量展开之后、
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
    Ok(merged)
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
    let content = serde_json::to_string_pretty(value).map_err(|e| McpConfigError::WriteError {
        path: path.display().to_string(),
        source: e.into(),
    })?;

    peri_mcp_config::write_text_atomic(path, &content).map_err(|e| McpConfigError::WriteError {
        path: path.display().to_string(),
        source: e,
    })
}

/// 从配置文件中删除指定的 MCP 服务器
/// 优先尝试项目级 .mcp.json，未找到则尝试全局 settings.json
pub fn remove_server_from_config(cwd: &Path, server_name: &str) -> Result<(), McpConfigError> {
    let global_path = peri_mcp_config::global_config_path();
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
    if config_exists(&project_path)? {
        let content =
            peri_mcp_config::read_text(&project_path).map_err(|e| McpConfigError::ReadError {
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
    if config_exists(global_path)? {
        let content =
            peri_mcp_config::read_text(global_path).map_err(|e| McpConfigError::ReadError {
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
    peri_config::mcp::parse_global(value)
        .map(|_| ())
        .map_err(|error| map_core_error(error, path))
}

/// 在配置文件中设置指定 MCP 服务器的 disabled 状态
/// 优先尝试项目级 .mcp.json，未找到则尝试全局 settings.json
pub fn set_server_disabled(
    cwd: &Path,
    server_name: &str,
    disabled: bool,
) -> Result<(), McpConfigError> {
    let global_path = peri_mcp_config::global_config_path();
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
    if config_exists(&project_path)? {
        let content =
            peri_mcp_config::read_text(&project_path).map_err(|e| McpConfigError::ReadError {
                path: project_path.display().to_string(),
                source: e,
            })?;

        let mut value: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| McpConfigError::ParseError {
                path: project_path.display().to_string(),
                source: e,
            })?;

        peri_config::mcp::parse_project(&value)
            .map_err(|error| map_core_error(error, &project_path))?;

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
            peri_config::mcp::parse_project(&value)
                .map_err(|error| map_core_error(error, &project_path))?;
            atomic_write_json(&project_path, &value)?;
            return Ok(());
        }
    }

    // 2. 尝试全局
    if config_exists(global_path)? {
        let content =
            peri_mcp_config::read_text(global_path).map_err(|e| McpConfigError::ReadError {
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

#[cfg(test)]
#[path = "config/io_test.rs"]
mod io_tests;

#[cfg(test)]
#[path = "config/policy_test.rs"]
mod policy_tests;

#[cfg(test)]
#[path = "config/cache_policy_test.rs"]
mod cache_policy_tests;

#[cfg(test)]
#[path = "config/snapshot_test.rs"]
mod snapshot_tests;
