use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use gray_matter::{engine::YAML, Matter};
use peri_lsp::config::{LspConfigSource, LspServerConfig};
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, warn};

use crate::{
    hooks::types::{HooksConfig, RegisteredHook},
    mcp::{config::McpConfigFile, McpServerConfig},
    plugin::{
        config::{
            load_claude_settings, load_installed_plugins, load_plugin_manifest,
            marketplaces_cache_dir, ClaudeSettings,
        },
        installer::generate_synthetic_manifest,
        marketplace::read_manifest_from_path,
        types::{InstalledPlugins, McpServerEntry, PluginCommandEntry, PluginManifest},
    },
    skills::{SkillRoot, SkillSource},
};

#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("插件清单加载失败: {0}")]
    ManifestLoadFailed(String),
    #[error("插件配置读取失败: {0}")]
    ConfigError(#[from] crate::plugin::PluginConfigError),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub enum CommandSource {
    Builtin,
    Plugin { path: PathBuf },
}

#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub source: CommandSource,
}

pub trait CommandProvider: Send + Sync {
    fn commands(&self) -> Vec<CommandEntry>;
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
pub struct CommandFrontmatter {
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    args: Option<serde_yaml::Value>,
}

pub fn parse_command_md(path: &Path) -> Option<(CommandFrontmatter, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let matter = Matter::<YAML>::new();
    let result: gray_matter::ParsedEntity = matter.parse(&content).ok()?;
    let fm: CommandFrontmatter = match result.data {
        Some(data) => data.deserialize().ok()?,
        None => CommandFrontmatter::default(),
    };
    Some((fm, result.content))
}

#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub name: String,
    pub version: String,
    pub install_path: PathBuf,
    pub manifest: PluginManifest,
    pub commands: Vec<CommandEntry>,
    pub skills_roots: Vec<SkillRoot>,
    pub agents_dirs: Vec<PathBuf>,
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// 插件数据目录（install_path/.claude-plugin/data），供 ${CLAUDE_PLUGIN_DATA} 展开
    pub data_path: PathBuf,
    /// 插件 hooks 配置（从 hooks/hooks.json 或 plugin.json hooks 字段提取）
    pub hooks_config: Option<HooksConfig>,
    /// 插件来源 marketplace（如 "claude-plugins-official"），用于追踪插件来源
    pub marketplace: String,
}

pub fn load_manifest(plugin_dir: &Path) -> Result<PluginManifest, LoaderError> {
    load_plugin_manifest(plugin_dir)
        .map_err(|e| LoaderError::ManifestLoadFailed(format!("{}: {e}", plugin_dir.display())))
}

/// 尝试从 marketplace manifest 中查找插件条目，生成合成 plugin.json 到插件缓存目录。
/// 返回 true 表示成功生成，false 表示无法生成（marketplace 不存在或插件条目未找到）。
fn try_generate_synthetic_manifest_fallback(
    install_path: &Path,
    plugin_name: &str,
    marketplace: &str,
) -> bool {
    if marketplace.is_empty() {
        return false;
    }

    let cache_dir = marketplaces_cache_dir().join(marketplace);
    let manifest_path = cache_dir.join("marketplace.json");
    let subdir_path = cache_dir.join(".claude-plugin").join("marketplace.json");

    let manifest_file = if manifest_path.exists() {
        manifest_path
    } else if subdir_path.exists() {
        subdir_path
    } else {
        return false;
    };

    let marketplace_manifest = match read_manifest_from_path(&manifest_file) {
        Ok(m) => m,
        Err(_) => return false,
    };

    let marketplace_plugin = match marketplace_manifest
        .plugins
        .iter()
        .find(|p| p.name == plugin_name)
    {
        Some(p) => p,
        None => return false,
    };

    // 只有当插件确实没有原生 plugin.json 时才生成
    let existing = install_path.join(".claude-plugin").join("plugin.json");
    if existing.exists() {
        return false;
    }

    match generate_synthetic_manifest(install_path, marketplace_plugin) {
        Ok(()) => {
            debug!(
                plugin = %plugin_name,
                marketplace = %marketplace,
                "已为旧缓存插件生成合成 plugin.json"
            );
            true
        }
        Err(e) => {
            warn!(
                plugin = %plugin_name,
                error = %e,
                "生成合成 plugin.json 失败"
            );
            false
        }
    }
}

pub(crate) fn extract_commands(
    manifest: &PluginManifest,
    base_dir: &Path,
    plugin_name: &str,
) -> Vec<CommandEntry> {
    let entries = match &manifest.commands {
        Some(cmds) if !cmds.is_empty() => cmds,
        _ => return Vec::new(),
    };

    let mut result = Vec::new();
    for entry in entries {
        match entry {
            PluginCommandEntry::Path(cmd_path) => {
                let full_path = base_dir.join(cmd_path);
                if !full_path.exists() {
                    warn!(path = %full_path.display(), "插件命令路径不存在，跳过");
                    continue;
                }
                if full_path.is_dir() {
                    // 目录：扫描所有 .md 文件
                    match std::fs::read_dir(&full_path) {
                        Ok(dir_entries) => {
                            for dir_entry in dir_entries.flatten() {
                                let p = dir_entry.path();
                                if p.extension().and_then(|e| e.to_str()) == Some("md") {
                                    process_command_file(&p, None, None, plugin_name, &mut result);
                                }
                            }
                        }
                        Err(e) => {
                            warn!(path = %full_path.display(), error = %e, "插件命令目录扫描失败，跳过");
                        }
                    }
                } else {
                    // 单个文件
                    process_command_file(&full_path, None, None, plugin_name, &mut result);
                }
            }
            PluginCommandEntry::Full(cmd) => {
                let cmd_file_path = base_dir.join(&cmd.path);
                if !cmd_file_path.exists() {
                    warn!(path = %cmd_file_path.display(), "插件命令文件不存在，跳过");
                    continue;
                }
                process_command_file(
                    &cmd_file_path,
                    cmd.name.as_deref(),
                    cmd.description.as_deref(),
                    plugin_name,
                    &mut result,
                );
            }
        }
    }
    result
}

fn process_command_file(
    cmd_file_path: &Path,
    explicit_name: Option<&str>,
    explicit_description: Option<&str>,
    plugin_name: &str,
    result: &mut Vec<CommandEntry>,
) {
    let (fm, _body) = match parse_command_md(cmd_file_path) {
        Some(parsed) => parsed,
        None => {
            warn!(path = %cmd_file_path.display(), "插件命令文件解析失败，跳过");
            return;
        }
    };

    let cmd_name = explicit_name.unwrap_or_else(|| {
        cmd_file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
    });

    let full_name = format!("{plugin_name}:{cmd_name}");
    let description = fm
        .description
        .or(explicit_description.map(String::from))
        .unwrap_or_default();

    result.push(CommandEntry {
        name: full_name,
        description,
        source: CommandSource::Plugin {
            path: cmd_file_path.to_path_buf(),
        },
    });
}

/// Extract skill roots from plugin manifest.
///
/// Manifest `skills` entries are treated as paths relative to the plugin root
/// (matching Claude Code convention: `skills: ["./skills/"]` or `skills: ["skills/tdd"]`).
/// Each entry becomes a `SkillRoot` (`source=Plugin`, `plugin_name=plugin_name`),
/// regardless of whether it directly contains `SKILL.md` or is a container——
/// `scan_skill_roots` handles both cases via leaf semantics.
///
/// Falls back to `base_dir/skills/` as a single root when no manifest skills are declared.
pub(crate) fn extract_skills_paths(
    manifest: &PluginManifest,
    base_dir: &Path,
    plugin_name: &str,
) -> Vec<SkillRoot> {
    let mut result = Vec::new();

    // 1. manifest 显式声明（每条 entry 是相对于插件根目录的路径）
    if let Some(skills) = &manifest.skills {
        if !skills.is_empty() {
            for entry in skills {
                let skill_path = base_dir.join(entry);
                if !skill_path.is_dir() {
                    debug!(path = %skill_path.display(), "插件 skill 路径不存在，跳过");
                    continue;
                }
                result.push(SkillRoot {
                    path: skill_path,
                    source: SkillSource::Plugin,
                    plugin_name: Some(plugin_name.to_string()),
                });
            }
            return result;
        }
    }

    // 2. fallback：base_dir/skills/ 作为一个 root（由 scan_skill_roots 递归扫描）
    let skills_dir = base_dir.join("skills");
    if skills_dir.is_dir() {
        result.push(SkillRoot {
            path: skills_dir,
            source: SkillSource::Plugin,
            plugin_name: Some(plugin_name.to_string()),
        });
    }

    result
}

/// Extract agent directories from plugin manifest.
///
/// When the manifest declares `agents`, uses those paths directly.
/// Falls back to scanning default directories (`agents/` and `.agents/`)
/// when no agents are declared — matching Claude Code's behavior where
/// agents placed in these directories are auto-discovered.
pub(crate) fn extract_agents_paths(manifest: &PluginManifest, base_dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();

    // 1. manifest 显式声明
    if let Some(agents) = &manifest.agents {
        if !agents.is_empty() {
            for agent in agents {
                let agent_path = base_dir.join(&agent.path);
                if agent_path.exists() {
                    result.push(agent_path);
                } else {
                    debug!(path = %agent_path.display(), "插件 agent 路径不存在，跳过");
                }
            }
            return result;
        }
    }

    // 2. fallback：扫描默认 agent 目录（agents/ 和 .agents/）
    for dir_name in &["agents", ".agents"] {
        let agents_dir = base_dir.join(dir_name);
        if agents_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&agents_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        result.push(p);
                    }
                }
            }
        }
    }

    result
}

/// Load MCP servers from a .mcp.json file, supporting both formats:
/// - Standard: `{"mcpServers": {...}}`
/// - Flat: `{"serverName": {...}}` (no mcpServers wrapper, used by context7/gitlab)
fn load_mcp_json_file(path: &Path) -> Option<HashMap<String, McpServerConfig>> {
    let content = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Try standard format first: {"mcpServers": {...}}
    if let Some(_servers) = v.get("mcpServers") {
        if let Ok(file_config) = serde_json::from_value::<McpConfigFile>(v.clone()) {
            if !file_config.mcp_servers.is_empty() {
                return Some(file_config.mcp_servers);
            }
        }
    }

    // Fallback: flat format — each key is a server name, value is a McpServerConfig
    if let Some(obj) = v.as_object() {
        let mut result = HashMap::new();
        for (key, val) in obj {
            // Skip known non-server keys
            if key == "mcpServers" {
                continue;
            }
            if let Ok(cfg) = serde_json::from_value::<McpServerConfig>(val.clone()) {
                result.insert(key.clone(), cfg);
            }
        }
        if !result.is_empty() {
            return Some(result);
        }
    }

    None
}

/// Extract MCP servers from plugin manifest.
/// Supports inline config objects and .mcp.json file path references.
/// Falls back to install_path/.mcp.json when manifest has no mcpServers.
pub(crate) fn extract_mcp_servers(
    manifest: &PluginManifest,
    install_path: &Path,
) -> HashMap<String, McpServerConfig> {
    let mut result = HashMap::new();

    if let Some(entries) = &manifest.mcp_servers {
        for (name, entry) in entries {
            match entry {
                McpServerEntry::Config(cfg) => {
                    result.insert(name.clone(), (**cfg).clone());
                }
                McpServerEntry::FilePath(path) => {
                    let resolved = install_path.join(path);
                    match load_mcp_json_file(&resolved) {
                        Some(mcp_servers) => {
                            for (srv_name, srv_cfg) in mcp_servers {
                                // 文件路径引用中的服务器名保留，外层会再加命名空间
                                let final_name = if srv_name == *name {
                                    // 如果只有一个服务器且与 key 同名，直接使用
                                    name.clone()
                                } else {
                                    format!("{}.{}", name, srv_name)
                                };
                                result.insert(final_name, srv_cfg);
                            }
                        }
                        None => {
                            warn!(
                                path = %resolved.display(),
                                "插件 MCP 配置文件加载失败，跳过"
                            );
                        }
                    }
                }
            }
        }
    }

    // Fallback: if manifest has no mcpServers, try install_path/.mcp.json
    if result.is_empty() {
        let mcp_json = install_path.join(".mcp.json");
        if mcp_json.exists() {
            debug!(path = %mcp_json.display(), "加载插件根目录 .mcp.json 作为 MCP 配置回退");
            if let Some(mcp_servers) = load_mcp_json_file(&mcp_json) {
                result = mcp_servers;
            }
        }
    }

    result
}

pub fn load_plugins(installed: &InstalledPlugins) -> Result<Vec<LoadedPlugin>, LoaderError> {
    let mut result = Vec::new();

    for plugin in &installed.plugins {
        let manifest = match load_manifest(&plugin.install_path) {
            Ok(m) => m,
            Err(_) => {
                // 尝试从 marketplace manifest 生成合成 plugin.json（兼容修复前安装的 LSP 插件）
                if try_generate_synthetic_manifest_fallback(
                    &plugin.install_path,
                    &plugin.name,
                    &plugin.marketplace,
                ) {
                    match load_manifest(&plugin.install_path) {
                        Ok(m) => m,
                        Err(_) => continue,
                    }
                } else {
                    continue;
                }
            }
        };

        let commands = extract_commands(&manifest, &plugin.install_path, &plugin.name);
        let skills_roots = extract_skills_paths(&manifest, &plugin.install_path, &plugin.name);
        let agents_dirs = extract_agents_paths(&manifest, &plugin.install_path);
        let mcp_servers = extract_mcp_servers(&manifest, &plugin.install_path);
        let data_path = plugin.install_path.join(".claude-plugin").join("data");
        let hooks_config = crate::hooks::loader::extract_hooks(&manifest, &plugin.install_path);

        result.push(LoadedPlugin {
            name: plugin.name.clone(),
            version: plugin.version.clone(),
            install_path: plugin.install_path.clone(),
            manifest,
            commands,
            skills_roots,
            agents_dirs,
            mcp_servers,
            data_path,
            hooks_config,
            marketplace: plugin.marketplace.clone(),
        });
    }

    debug!(count = result.len(), "已加载插件");
    Ok(result)
}

/// 合并用户级和项目级的 enabledPlugins
///
/// 规则：
/// 1. 项目级不存在 → 用用户级
/// 2. 项目级 enabledPlugins 为空 → 沿用用户级
/// 3. 项目级非空 → 以项目为准（完全替换，与 Claude Code 行为一致）
fn merge_enabled_plugins(
    user: &ClaudeSettings,
    project: Option<&ClaudeSettings>,
) -> HashSet<String> {
    let Some(project) = project else {
        return user.enabled_plugins.iter().cloned().collect();
    };

    // 项目级 enabledPlugins 为空 → 沿用用户级
    if project.enabled_plugins.is_empty() {
        return user.enabled_plugins.iter().cloned().collect();
    }

    // 项目级非空 → 完全替换
    project.enabled_plugins.iter().cloned().collect()
}

pub fn load_enabled_plugins(
    claude_dir: &Path,
    cwd: Option<&Path>,
) -> Result<Vec<LoadedPlugin>, LoaderError> {
    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let settings_path = claude_dir.join("settings.json");

    let installed = load_installed_plugins(Some(&plugins_path))?;
    let user_settings = load_claude_settings(Some(&settings_path))?;

    // 尝试加载项目级 settings.json（与 P0-1 hooks 加载一致）
    let project_settings = cwd
        .map(|p| p.join(".claude").join("settings.json"))
        .filter(|p| p.exists())
        .and_then(|p| load_claude_settings(Some(&p)).ok());

    let enabled_ids = merge_enabled_plugins(&user_settings, project_settings.as_ref());

    let filtered: Vec<_> = installed
        .plugins
        .into_iter()
        .filter(|p| enabled_ids.contains(&p.id))
        .collect();

    let filtered_installed = InstalledPlugins {
        version: installed.version,
        plugins: filtered,
    };

    load_plugins(&filtered_installed)
}

pub struct PluginCommandProvider {
    entries: Vec<CommandEntry>,
}

impl PluginCommandProvider {
    pub fn new(plugins: &[LoadedPlugin]) -> Self {
        let entries: Vec<CommandEntry> = plugins.iter().flat_map(|p| p.commands.clone()).collect();
        Self { entries }
    }
}

impl CommandProvider for PluginCommandProvider {
    fn commands(&self) -> Vec<CommandEntry> {
        self.entries.clone()
    }
}

pub fn merge_plugin_mcp_servers(plugins: &[LoadedPlugin]) -> HashMap<String, McpServerConfig> {
    let mut result = HashMap::new();
    for plugin in plugins {
        for (name, config) in &plugin.mcp_servers {
            // 与 Claude Code 一致：使用 plugin:{插件名}:{服务器名} 前缀
            let namespaced = format!("plugin:{}:{}", plugin.name, name);
            result.insert(namespaced, config.clone());
        }
    }
    result
}

/// 所有已启用插件的聚合加载结果
#[derive(Debug, Clone)]
pub struct PluginLoadResult {
    pub plugins: Vec<LoadedPlugin>,
    pub all_skill_roots: Vec<SkillRoot>,
    pub all_mcp_servers: HashMap<String, McpServerConfig>,
    pub all_agent_dirs: Vec<PathBuf>,
    pub all_commands: Vec<CommandEntry>,
    pub all_hooks: Vec<RegisteredHook>,
    /// 聚合所有插件的 LSP 服务器配置
    pub all_lsp_servers: Vec<LspServerConfig>,
}

/// 加载所有已启用插件，返回聚合结果（skills 路径、MCP 服务器、agent 路径、命令列表）
///
/// `cwd`：项目工作目录，用于发现项目级 `.claude/settings.json` 的 `enabledPlugins`。
/// 传 `None` 时仅读取用户级 `~/.claude/settings.json`。
pub fn load_enabled_plugins_aggregated(claude_dir: &Path, cwd: Option<&Path>) -> PluginLoadResult {
    let plugins = match load_enabled_plugins(claude_dir, cwd) {
        Ok(p) => p,
        Err(_) => {
            // 静默失败，避免在 TUI 上打印错误日志
            return PluginLoadResult {
                plugins: vec![],
                all_skill_roots: vec![],
                all_mcp_servers: HashMap::new(),
                all_agent_dirs: vec![],
                all_commands: vec![],
                all_hooks: vec![],
                all_lsp_servers: vec![],
            };
        }
    };

    let all_skill_roots: Vec<SkillRoot> = plugins
        .iter()
        .flat_map(|p| p.skills_roots.clone())
        .collect();

    let all_mcp_servers = merge_plugin_mcp_servers(&plugins);

    let all_agent_dirs: Vec<PathBuf> = plugins.iter().flat_map(|p| p.agents_dirs.clone()).collect();

    let all_commands: Vec<CommandEntry> = plugins.iter().flat_map(|p| p.commands.clone()).collect();

    let all_hooks: Vec<RegisteredHook> = plugins
        .iter()
        .filter_map(|plugin| {
            let config = plugin.hooks_config.as_ref()?;
            let mut hooks = Vec::new();
            for (event, matchers) in config {
                for rule in matchers {
                    for hook_def in &rule.hooks {
                        hooks.push(RegisteredHook {
                            hook: hook_def.clone(),
                            event: event.clone(),
                            matcher: rule
                                .matcher
                                .clone()
                                .or_else(|| hook_def.get_matcher().cloned()),
                            plugin_name: plugin.name.clone(),
                            plugin_id: plugin.name.clone(),
                            plugin_root: plugin.install_path.clone(),
                            plugin_data_dir: plugin.data_path.clone(),
                            plugin_options: plugin
                                .manifest
                                .options
                                .as_ref()
                                .unwrap_or(&vec![])
                                .iter()
                                .filter_map(|opt| {
                                    opt.default.as_ref().map(|v| (opt.name.clone(), v.clone()))
                                })
                                .collect(),
                        });
                    }
                }
            }
            Some(hooks)
        })
        .flatten()
        .collect();

    let all_lsp_servers: Vec<LspServerConfig> = plugins
        .iter()
        .filter_map(|plugin| {
            let servers = plugin.manifest.lsp_servers.as_ref()?;
            if servers.is_empty() {
                return None;
            }
            Some(
                servers
                    .iter()
                    .map(|s| LspServerConfig {
                        name: s.name.clone(),
                        command: s.command.clone(),
                        args: s.args.clone(),
                        env: None,
                        extension_to_language: s.extension_to_language.clone(),
                        initialization_options: None,
                        disabled: None,
                        max_restarts: None,
                        startup_timeout: None,
                        source: Some(LspConfigSource::Plugin {
                            plugin_name: plugin.name.clone(),
                        }),
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();

    PluginLoadResult {
        plugins,
        all_skill_roots,
        all_mcp_servers,
        all_agent_dirs,
        all_commands,
        all_hooks,
        all_lsp_servers,
    }
}

#[cfg(test)]
#[path = "loader_test.rs"]
pub(crate) mod tests;
