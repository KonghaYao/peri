use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use gray_matter::{engine::YAML, Matter};
use peri_acp_types::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource as RouteCommandSource,
    RouteEntry,
};
use peri_acp_types::command::{CommandContext, CommandHandler, CommandOutcome};
use peri_acp_types::plugin::McpServerConfigValidationError;
use peri_resources::lsp::config::{lsp_config_from_plugin, LspServerConfig};
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, warn};

use crate::{
    hooks::types::RegisteredHook,
    mcp::McpServerConfig,
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

// 3.0 批 2 波 1：协议类型归契约层（定义见 `peri_acp_types::plugin`）。
// `CommandSource` / `CommandEntry` / `LoadedPlugin` / `PluginLoadResult` 自本文件
// 迁出；本模块保留 re-export 保兼容。`CommandProvider` 随迁（trait 引用迁出类型）。
pub use peri_acp_types::plugin::{
    CommandEntry, CommandProvider, CommandSource, LoadedPlugin, PluginLoadResult,
};

#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("插件清单加载失败: {0}")]
    ManifestLoadFailed(String),
    #[error("插件配置读取失败: {0}")]
    ConfigError(#[from] crate::plugin::PluginConfigError),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 插件 MCP 配置无效（MCP 专用严格路径）。
    ///
    /// `message` 只保留固定规则正文或解析定位（行列/错误类别），不回显原始输入值
    /// ——env / headers / OAuth 字段的内容不得进入错误文本（ARC-SECRET-001）。
    #[error("插件 MCP 配置无效: {path}: {message}")]
    McpConfigInvalid { path: PathBuf, message: String },
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

    // 与 CommandSource::Plugin 语义对齐（namespace = 插件名）：`plugin:{plugin}:{cmd}`
    // 三层形态——原 `{plugin}:{cmd}` 二层形态对第二等级（外部来源）非法，必须显式 plugin 域前缀。
    let full_name = format!("plugin:{plugin_name}:{cmd_name}");
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

/// 插件命令占位 handler（Phase 6 B2；设计「正交维度」：外部系统命令不改变
/// 执行通路，仅要求路由表支持运行时注册 / 注销）。
///
/// 占位实现（执行语义未定）：返回 [`CommandOutcome::Inject`] 空串——拦截
/// 路径对 Inject 的既有处理为 warn + fall-through（原文进 agent 管线，
/// 命令不被吞，与 `mcp/skill_discovery.rs` 的 `McpSkillPlaceholder` /
/// `peri-acp` 的 `PassthroughPlaceholder` 同构）。UI-only 反馈「插件命令
/// 执行待后续版本」与正式执行体留待 Phase 5+ 补齐（注册 / 注销 / 投影
/// 链路本 Phase 全量生效）。
#[derive(Clone)]
pub struct PluginCommandHandler {
    /// 命令来源（插件命令文件路径；占位期仅承载来源信息）。
    pub source: CommandSource,
}

#[async_trait]
impl CommandHandler for PluginCommandHandler {
    async fn execute(&self, _ctx: CommandContext) -> CommandOutcome {
        // 占位：Inject 空串 → 拦截路径 fall-through，原文进 agent 管线。
        CommandOutcome::Inject(String::new())
    }
}

/// `CommandEntry` → `RouteEntry`（plugin 域；name 形如 `plugin:{plugin}:{cmd}`，
/// B1 词法迁移后的三层形态，设计 §44-59 第二等级）。
///
/// fullname 原样使用（`plugin:{plugin}:{cmd}`）；kind = [`CommandEntryKind::Command`]
/// （plugin 域暂归 Command，设计 §85 注）；provenance = `CommandSource::Plugin`
/// （**剥离 `plugin:` 前缀**的插件名——register 域校验将核对词法 namespace
/// 段 == 插件名，设计 §58，未剥离 → ProvenanceMismatch 全量拒绝）+
/// [`CommandLifecycle::Connected`]（静态装配，与 MCP 动态注入的 Discovered
/// 相对）；handler = [`PluginCommandHandler`] 占位。含 `plugin:` 前缀但缺
/// 末段 cmd（词法异常，如 `plugin:x`）→ 跳过并告警；非 plugin 域 name
/// （如 `foo:bar`）不属本函数职责，静默跳过（register 词法校验兜底）。
pub fn plugin_route_entries(entries: &[CommandEntry]) -> Vec<RouteEntry> {
    entries
        .iter()
        .filter_map(|e| {
            // 先剥 "plugin:" 域前缀（非 plugin 域 → 静默跳过），再取末段
            // cmd：`plugin:ecc:deploy` → ("ecc", "deploy")；`plugin:x`（单层
            // 非法）→ None → 告警跳过（register 词法校验兜底）。
            let rest = e.name.strip_prefix("plugin:")?;
            let Some((plugin, _cmd)) = rest.rsplit_once(':') else {
                warn!(name = %e.name, "插件命令名词法异常，跳过 plugin 域注册");
                return None;
            };
            Some(RouteEntry {
                fullname: e.name.clone(), // "plugin:{plugin}:{cmd}"
                aliases: vec![],
                description: e.description.clone(),
                kind: CommandEntryKind::Command, // plugin 域暂归 Command（设计 §85 注）
                category: None,
                args_schema: None,
                handler: Arc::new(PluginCommandHandler {
                    source: e.source.clone(),
                }),
                provenance: CommandProvenance {
                    source: RouteCommandSource::Plugin {
                        name: plugin.to_string(),
                    },
                    lifecycle: CommandLifecycle::Connected,
                },
            })
        })
        .collect()
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

/// 插件清单文件路径（`.claude-plugin/plugin.json`）。
fn plugin_manifest_path(install_path: &Path) -> PathBuf {
    install_path.join(".claude-plugin").join("plugin.json")
}

/// 解析错误的可诊断定位：只保留错误类别与行列，不回显原始输入值
/// （serde 的 `invalid type` 正文会带上值本身，可能把 env / headers 内容写进日志）。
fn describe_json_error(error: &serde_json::Error) -> String {
    let kind = match error.classify() {
        serde_json::error::Category::Io => "I/O 错误",
        serde_json::error::Category::Syntax => "JSON 语法错误",
        serde_json::error::Category::Data => "字段类型或取值不符合契约",
        serde_json::error::Category::Eof => "JSON 提前结束",
    };
    format!("{kind}（行 {} 列 {}）", error.line(), error.column())
}

/// 契约层冻结的 System 规则正文：命中即回显固定规则文本，否则退回解析定位。
///
/// 这里匹配的是本仓库自己的冻结错误文案（`McpServerConfigValidationError` 的
/// Display），不是任意用户输入；命中与否只决定错误文本，不决定跳过或继续。
fn describe_server_parse_error(error: &serde_json::Error) -> String {
    let text = error.to_string();
    let rule = [
        McpServerConfigValidationError::SystemMcpToolsRequiresSystemMcp,
        McpServerConfigValidationError::SystemMcpTimeoutRequiresSystemMcp,
        McpServerConfigValidationError::SystemMcpTimeoutOutOfRange,
    ]
    .into_iter()
    .map(|rule| rule.to_string())
    .find(|rule| text.contains(rule));
    rule.unwrap_or_else(|| describe_json_error(error))
}

/// 解析单个 MCP server 条目：typed 反序列化（含 System 组合校验）后再做纯校验。
fn parse_mcp_server_entry(
    value: &serde_json::Value,
    path: &Path,
    server_name: &str,
) -> Result<McpServerConfig, LoaderError> {
    let config: McpServerConfig =
        serde_json::from_value(value.clone()).map_err(|error| LoaderError::McpConfigInvalid {
            path: path.to_path_buf(),
            message: format!("{server_name}: {}", describe_server_parse_error(&error)),
        })?;
    validate_mcp_server_config(&config, path, server_name)?;
    Ok(config)
}

/// 单个 server 配置的纯校验（含手工构造的 typed 配置）。
fn validate_mcp_server_config(
    config: &McpServerConfig,
    path: &Path,
    server_name: &str,
) -> Result<(), LoaderError> {
    config
        .validate()
        .map_err(|rule| LoaderError::McpConfigInvalid {
            path: path.to_path_buf(),
            message: format!("{server_name}: {rule}"),
        })
}

/// 解析 `{"serverName": {...}}` 形态的 server map；任一 entry 非法即整体失败。
fn parse_mcp_servers_object(
    value: &serde_json::Value,
    path: &Path,
) -> Result<HashMap<String, McpServerConfig>, LoaderError> {
    let Some(object) = value.as_object() else {
        return Err(LoaderError::McpConfigInvalid {
            path: path.to_path_buf(),
            message: "mcpServers 必须是对象".to_string(),
        });
    };
    let mut result = HashMap::new();
    for (name, entry) in object {
        result.insert(name.clone(), parse_mcp_server_entry(entry, path, name)?);
    }
    Ok(result)
}

/// Load MCP servers from a .mcp.json file, supporting both formats:
/// - Standard: `{"mcpServers": {...}}`
/// - Flat: `{"serverName": {...}}` (no mcpServers wrapper, used by context7/gitlab)
///
/// 严格语义：缺失文件是「未声明」（`Ok(None)`），存在但读取/解析失败是错误
/// （`Err`），不当作可跳过的条目。wrapped 形态一旦出现就不再看 flat 形态；
/// flat 形态任一 entry 非法则整体失败，不保留部分成功。
fn load_mcp_json_file(
    path: &Path,
) -> Result<Option<HashMap<String, McpServerConfig>>, LoaderError> {
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path).map_err(|error| LoaderError::McpConfigInvalid {
        path: path.to_path_buf(),
        message: format!("读取失败: {error}"),
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|error| LoaderError::McpConfigInvalid {
            path: path.to_path_buf(),
            message: describe_json_error(&error),
        })?;

    // Standard format: {"mcpServers": {...}}（空 map 也是「已声明」）
    if let Some(servers) = value.get("mcpServers") {
        return Ok(Some(parse_mcp_servers_object(servers, path)?));
    }

    // Flat format — each key is a server name, value is a McpServerConfig
    Ok(Some(parse_mcp_servers_object(&value, path)?))
}

/// Extract MCP servers from plugin manifest.
/// Supports inline config objects and .mcp.json file path references.
/// Falls back to install_path/.mcp.json when manifest has no mcpServers.
///
/// 严格语义：manifest 声明了 `mcpServers`（包括空 map）就不回退根 `.mcp.json`；
/// 回退只在**未声明**时发生，不因解析失败而触发。内联条目逐项校验；被引用的
/// 配置文件非法时整个提取失败，不部分接纳合法兄弟条目。
pub(crate) fn extract_mcp_servers(
    manifest: &PluginManifest,
    install_path: &Path,
) -> Result<HashMap<String, McpServerConfig>, LoaderError> {
    let mut result = HashMap::new();

    if let Some(entries) = &manifest.mcp_servers {
        let manifest_path = plugin_manifest_path(install_path);
        for (name, entry) in entries {
            match entry {
                McpServerEntry::Config(cfg) => {
                    validate_mcp_server_config(cfg, &manifest_path, name)?;
                    result.insert(name.clone(), (**cfg).clone());
                }
                McpServerEntry::FilePath(path) => {
                    let resolved = install_path.join(path);
                    match load_mcp_json_file(&resolved)? {
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
                                "插件 MCP 配置文件不存在，跳过该声明"
                            );
                        }
                    }
                }
            }
        }
        return Ok(result);
    }

    // Fallback: if manifest has no mcpServers, try install_path/.mcp.json
    let mcp_json = install_path.join(".mcp.json");
    if !mcp_json.exists() {
        return Ok(result);
    }
    debug!(path = %mcp_json.display(), "加载插件根目录 .mcp.json 作为 MCP 配置回退");
    Ok(load_mcp_json_file(&mcp_json)?.unwrap_or_default())
}

/// 插件装配时对 MCP 配置错误采用的处理策略。
///
/// 严格化**只限 MCP 启动路径**：既有宽容 API（`load_enabled_plugins_aggregated`
/// 等展示/聚合入口）保持「坏插件不阻止宿主启动」的产品行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpConfigPolicy {
    /// 宽容（展示 / 面板 / skills / hooks 聚合）：记录安全诊断，该插件的 MCP 声明按空处理。
    Lenient,
    /// 严格（MCP 启动路径）：非法 MCP 配置直接失败，不降级为空配置。
    Strict,
}

pub fn load_plugins(installed: &InstalledPlugins) -> Result<Vec<LoadedPlugin>, LoaderError> {
    load_plugins_with_policy(installed, McpConfigPolicy::Lenient)
}

/// 装配已安装插件；`policy` 决定非法 MCP 配置是失败还是降级。
fn load_plugins_with_policy(
    installed: &InstalledPlugins,
    policy: McpConfigPolicy,
) -> Result<Vec<LoadedPlugin>, LoaderError> {
    let mut result = Vec::new();

    for plugin in &installed.plugins {
        let manifest = match load_manifest(&plugin.install_path) {
            Ok(m) => m,
            Err(error) => {
                let manifest_path = plugin_manifest_path(&plugin.install_path);
                // 已存在但非法的清单不允许被合成清单覆盖修复，也不当作未安装：
                // 严格路径直接报错，宽容路径记录诊断后跳过该插件。
                if manifest_path.exists() {
                    if policy == McpConfigPolicy::Strict {
                        return Err(error);
                    }
                    warn!(
                        plugin = %plugin.name,
                        error = %error,
                        "插件清单非法，跳过该插件"
                    );
                    continue;
                }
                // 清单文件缺失：允许从 marketplace manifest 生成合成清单
                // （兼容修复前安装的 LSP 插件），生成结果同样按严格语义解析。
                if !try_generate_synthetic_manifest_fallback(
                    &plugin.install_path,
                    &plugin.name,
                    &plugin.marketplace,
                ) {
                    warn!(
                        plugin = %plugin.name,
                        "插件清单缺失且无法生成合成清单，跳过该插件"
                    );
                    continue;
                }
                match load_manifest(&plugin.install_path) {
                    Ok(m) => m,
                    Err(error) => {
                        if policy == McpConfigPolicy::Strict {
                            return Err(error);
                        }
                        warn!(
                            plugin = %plugin.name,
                            error = %error,
                            "合成清单解析失败，跳过该插件"
                        );
                        continue;
                    }
                }
            }
        };

        let commands = extract_commands(&manifest, &plugin.install_path, &plugin.name);
        let skills_roots = extract_skills_paths(&manifest, &plugin.install_path, &plugin.name);
        let agents_dirs = extract_agents_paths(&manifest, &plugin.install_path);
        let mcp_servers = match extract_mcp_servers(&manifest, &plugin.install_path) {
            Ok(servers) => servers,
            Err(error) => match policy {
                McpConfigPolicy::Strict => return Err(error),
                McpConfigPolicy::Lenient => {
                    warn!(
                        plugin = %plugin.name,
                        error = %error,
                        "插件 MCP 配置无效，跳过该插件的 MCP 声明"
                    );
                    HashMap::new()
                }
            },
        };
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

/// 选出已启用插件（installed 记录 ∩ enabledPlugins）。
///
/// 严格与宽容入口共用本函数：启用范围规则只有一份，两条路径不复制解析逻辑。
fn select_enabled_plugins(
    claude_dir: &Path,
    cwd: Option<&Path>,
) -> Result<InstalledPlugins, LoaderError> {
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

    Ok(InstalledPlugins {
        version: installed.version,
        plugins: filtered,
    })
}

pub fn load_enabled_plugins(
    claude_dir: &Path,
    cwd: Option<&Path>,
) -> Result<Vec<LoadedPlugin>, LoaderError> {
    load_plugins(&select_enabled_plugins(claude_dir, cwd)?)
}

/// MCP 执行专用严格入口：复用启用选择与装配逻辑，但非法 MCP 配置直接失败
/// ——不降级为空配置、不当作未安装继续。
///
/// 只有 MCP 合并（`mcp::config`）使用本入口；`load_enabled_plugins_aggregated`
/// 等宽容展示 API 的类型与行为保持不变。
pub(crate) fn load_enabled_plugins_for_mcp(
    claude_dir: &Path,
    cwd: Option<&Path>,
) -> Result<Vec<LoadedPlugin>, LoaderError> {
    load_plugins_with_policy(
        &select_enabled_plugins(claude_dir, cwd)?,
        McpConfigPolicy::Strict,
    )
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
            // config 层唯一键（与 Claude Code 一致）：`plugin:{插件名}:{服务器名}`
            // 不进命令命名空间——命令词法层（plugin 域 `plugin:{plugin}:{cmd}`）与
            // MCP server 键各自独立，互不交叉。
            let namespaced = format!("plugin:{}:{}", plugin.name, name);
            result.insert(namespaced, config.clone());
        }
    }
    result
}

/// 所有已启用插件的聚合加载结果

/// 加载所有已启用插件，返回聚合结果（skills 路径、MCP 服务器、agent 路径、命令列表）
///
/// `cwd`：项目工作目录，用于发现项目级 `.claude/settings.json` 的 `enabledPlugins`。
/// 传 `None` 时仅读取用户级 `~/.claude/settings.json`。
pub fn load_enabled_plugins_aggregated(claude_dir: &Path, cwd: Option<&Path>) -> PluginLoadResult {
    let plugins = match load_enabled_plugins(claude_dir, cwd) {
        Ok(p) => p,
        Err(error) => {
            // 宽容展示路径：保留「返回空结果」的产品行为，但错误必须可见
            // ——不静默丢弃（合法诊断只含路径与固定规则/解析定位）。
            warn!(
                claude_dir = %claude_dir.display(),
                error = %error,
                "插件聚合加载失败，返回空结果"
            );
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
                    .map(|s| {
                        lsp_config_from_plugin(
                            &plugin.name,
                            &s.name,
                            &s.command,
                            &s.args,
                            &plugin.install_path,
                            s.extension_to_language.clone(),
                        )
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
