//! 装配注入端口实现（3.0 批 2 波 2）。
//!
//! ACP 协议面只持 `peri_acp_types` 端口接口；具体实现（包装 middlewares
//! 业务函数）归实现方本模块。宿主装配点构造本模块实现后 upcast 注入。

use std::path::{Path, PathBuf};

use peri_acp_types::agents::AgentCapability;
use peri_acp_types::event_data::PluginSnapshotEntry;
use peri_acp_types::hooks::SettingsHooksPort;
use peri_acp_types::plugin::{InstallScope, InstalledPlugin, PluginManagerPort};
use peri_acp_types::ports::SkillsPort;
use peri_acp_types::skills::{SkillMetadata, SkillRoot};

use crate::plugin::{
    cleanup_orphaned_plugins, install_plugin, load_installed_plugins, load_known_marketplaces,
    parse_marketplace_input, remove_from_enabled_plugins, save_known_marketplaces,
    uninstall_plugin, update_enabled_plugins, update_plugin, KnownMarketplace, MarketplaceManager,
    MarketplaceSource,
};

/// 插件管理端口实现：包装 `install_plugin` / `uninstall_plugin` /
/// `update_enabled_plugins` / `remove_from_enabled_plugins` /
/// `update_plugin` / marketplace 刷新 / 聚合快照。
#[derive(Debug, Clone, Copy, Default)]
pub struct PluginManager;

#[async_trait::async_trait]
impl PluginManagerPort for PluginManager {
    async fn install(
        &self,
        name: &str,
        marketplace: &str,
        scope: InstallScope,
        cache_dir: &Path,
        claude_dir: &Path,
    ) -> Result<InstalledPlugin, String> {
        install_plugin(name, marketplace, scope, cache_dir, claude_dir, None)
            .await
            .map_err(|e| e.to_string())
    }

    async fn uninstall(&self, plugin_id: &str, claude_dir: &Path) -> Result<(), String> {
        uninstall_plugin(plugin_id, claude_dir, None)
            .await
            .map_err(|e| e.to_string())
    }

    fn set_enabled(
        &self,
        plugin_id: &str,
        scope: InstallScope,
        claude_dir: &Path,
        enable: bool,
    ) -> Result<(), String> {
        if enable {
            update_enabled_plugins(plugin_id, scope, claude_dir, None)
        } else {
            remove_from_enabled_plugins(plugin_id, &scope, claude_dir, None)
        }
        .map_err(|e| e.to_string())
    }

    fn cache_dir(&self) -> PathBuf {
        crate::plugin::config::marketplaces_cache_dir()
    }

    async fn update(
        &self,
        plugin_id: &str,
        cache_dir: &Path,
        claude_dir: &Path,
    ) -> Result<InstalledPlugin, String> {
        update_plugin(plugin_id, cache_dir, claude_dir, None)
            .await
            .map_err(|e| e.to_string())
    }

    async fn refresh_marketplace(&self, name: &str) -> Result<usize, String> {
        let kms = load_known_marketplaces(None)
            .map_err(|e| format!("Failed to load marketplaces: {e}"))?;
        let km = kms
            .iter()
            .find(|km| crate::plugin::MarketplaceManager::extract_name(&km.source) == name)
            .ok_or_else(|| format!("marketplace not found: {name}"))?;
        let (manifest, _install_location) =
            crate::plugin::marketplace::refresh_marketplace(&km.source, name)
                .await
                .map_err(|e| e.to_string())?;
        Ok(manifest.plugins.len())
    }

    fn snapshot(&self, claude_dir: &Path) -> Vec<PluginSnapshotEntry> {
        let loaded = crate::plugin::load_enabled_plugins_aggregated(claude_dir, None);

        let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
        let installed = crate::plugin::load_installed_plugins(Some(&plugins_path))
            .ok()
            .unwrap_or_default();

        loaded
            .plugins
            .iter()
            .map(|p| PluginSnapshotEntry {
                name: p.manifest.name.clone(),
                version: p.manifest.version.clone(),
                enabled: installed.plugins.iter().any(|ip| ip.name == p.name),
                root: p.install_path.to_string_lossy().to_string(),
                description: p.manifest.description.clone(),
                marketplace: p.marketplace.clone(),
                author: p.manifest.author.as_ref().map(|a| a.name.clone()),
                skills_count: p.skills_roots.len(),
                commands_count: p.commands.len(),
                agents_count: p.agents_dirs.len(),
                mcp_count: p.mcp_servers.len(),
                install_scope: installed
                    .plugins
                    .iter()
                    .find(|ip| ip.name == p.name)
                    .map(|ip| format!("{:?}", ip.scope).to_lowercase())
                    .unwrap_or_default(),
                load_error: None,
            })
            .collect()
    }

    async fn cleanup(&self, claude_dir: &Path) -> Result<usize, String> {
        cleanup_orphaned_plugins(claude_dir)
            .await
            .map_err(|e| e.to_string())
    }

    async fn marketplace_add(&self, source: &str) -> Result<String, String> {
        // 与迁移前 TUI `cli_plugin::run_marketplace_add` 逻辑一致：
        // 解析 → 去重（install_location 非空视为真重复）→ clone/fetch →
        // 记录 known_marketplaces。
        let marketplace_source = parse_marketplace_input(source)?;
        let name = MarketplaceManager::extract_name(&marketplace_source);

        let mut marketplaces = load_known_marketplaces(None).map_err(|e| e.to_string())?;
        if let Some(existing) = marketplaces
            .iter()
            .position(|mkt| MarketplaceManager::extract_name(&mkt.source) == name)
        {
            let old = &marketplaces[existing];
            if !old.install_location.is_empty() {
                return Ok(name);
            }
            // 旧残留（install_location 为空），删除后重新添加
            marketplaces.remove(existing);
        }

        let (manifest, install_location) =
            crate::plugin::marketplace::refresh_marketplace(&marketplace_source, &name)
                .await
                .map_err(|e| e.to_string())?;
        let actual_name = manifest.name;

        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        marketplaces.push(KnownMarketplace {
            source: marketplace_source,
            install_location,
            auto_update: false,
            last_updated: now,
        });
        save_known_marketplaces(&marketplaces, None).map_err(|e| e.to_string())?;
        Ok(actual_name)
    }

    async fn marketplace_remove(&self, name: &str) -> Result<(), String> {
        // 与迁移前 TUI `cli_plugin::run_marketplace_remove` 逻辑一致：
        // 过滤 known_marketplaces + 清除磁盘缓存目录。
        let marketplaces = load_known_marketplaces(None).map_err(|e| e.to_string())?;
        let original_len = marketplaces.len();

        let removed_location = marketplaces
            .iter()
            .find(|mkt| MarketplaceManager::extract_name(&mkt.source) == name)
            .map(|km| km.install_location.clone());

        let filtered: Vec<KnownMarketplace> = marketplaces
            .into_iter()
            .filter(|mkt| MarketplaceManager::extract_name(&mkt.source) != name)
            .collect();

        if filtered.len() == original_len {
            return Err(format!("未找到名为 \"{name}\" 的 marketplace"));
        }

        save_known_marketplaces(&filtered, None).map_err(|e| e.to_string())?;
        if let Some(loc) = removed_location {
            let install_path = std::path::Path::new(&loc);
            if !loc.is_empty() && install_path.exists() {
                std::fs::remove_dir_all(install_path).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    async fn marketplace_update(&self, name: &str) -> Result<String, String> {
        // 与迁移前 TUI `cli_plugin::run_marketplace_update` 逻辑一致。
        let marketplaces = load_known_marketplaces(None).map_err(|e| e.to_string())?;
        let entry_index = marketplaces
            .iter()
            .position(|mkt| MarketplaceManager::extract_name(&mkt.source) == name)
            .ok_or_else(|| format!("未找到名为 \"{name}\" 的 marketplace"))?;

        let entry = &marketplaces[entry_index];
        let (manifest, install_location) =
            crate::plugin::marketplace::refresh_marketplace(&entry.source, name)
                .await
                .map_err(|e| e.to_string())?;

        let mut updated = marketplaces;
        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        updated[entry_index].install_location = install_location;
        updated[entry_index].last_updated = now;

        save_known_marketplaces(&updated, None).map_err(|e| e.to_string())?;
        Ok(manifest.name)
    }

    fn marketplace_snapshot(&self) -> serde_json::Value {
        // 面板数据快照：派生逻辑与迁移前 TUI 面板
        // `load_marketplace_data` + `load_discover_plugins_from_disk` 一致
        // （known marketplaces × 缓存 manifest × installed 记录）。
        let known = load_known_marketplaces(None).unwrap_or_default();
        let cache_dir = crate::plugin::marketplaces_cache_dir();
        let _ = std::fs::create_dir_all(&cache_dir);
        let installed = load_installed_plugins(None).unwrap_or_default();

        let marketplaces: Vec<serde_json::Value> = known
            .iter()
            .map(|km| {
                let name = MarketplaceManager::extract_name(&km.source);
                let cache_path = cache_dir.join(&name);
                let manifest_path = crate::plugin::marketplace::find_marketplace_json(&cache_path);
                let mut status = if km.install_location.is_empty() || manifest_path.is_none() {
                    "not_found"
                } else {
                    "cached"
                };
                // B3: 检查 manifest mtime，超过 24h 标记为 Stale
                if status == "cached" {
                    if let Some(ref path) = manifest_path {
                        if let Ok(meta) = std::fs::metadata(path) {
                            if let Ok(mtime) = meta.modified() {
                                if let Ok(elapsed) = mtime.elapsed() {
                                    if elapsed.as_secs() > 24 * 3600 {
                                        status = "stale";
                                    }
                                }
                            }
                        }
                    }
                }

                // 从 cached manifest 统计插件数
                let mut manifest_parse_failed = false;
                let plugin_count = match manifest_path.as_ref() {
                    Some(path) => {
                        if let Ok(content) = std::fs::read_to_string(path) {
                            if let Ok(manifest) =
                                serde_json::from_str::<serde_json::Value>(&content)
                            {
                                manifest
                                    .get("plugins")
                                    .and_then(|v| v.as_array())
                                    .map(|a| a.len())
                                    .unwrap_or(0)
                            } else {
                                manifest_parse_failed = true;
                                0
                            }
                        } else {
                            manifest_parse_failed = true;
                            0
                        }
                    }
                    None => 0,
                };
                if manifest_parse_failed {
                    status = "failed";
                }

                // 统计已安装的插件数（来自此 marketplace）
                let installed_count = installed
                    .plugins
                    .iter()
                    .filter(|p| {
                        let mp = if let Some((_, mkt)) = p.id.split_once('@') {
                            mkt
                        } else {
                            ""
                        };
                        mp == name
                    })
                    .count();

                serde_json::json!({
                    "name": name,
                    "source_label": match &km.source {
                        MarketplaceSource::GitHub { repo } => format!("github:{repo}"),
                        MarketplaceSource::Git { url } => format!("git:{url}"),
                        MarketplaceSource::Url { url } => url.clone(),
                        MarketplaceSource::Directory { path } => path.clone(),
                        MarketplaceSource::File { path } => path.clone(),
                        MarketplaceSource::Npm { package } => format!("npm:{package}"),
                    },
                    "plugin_count": plugin_count,
                    "installed_count": installed_count,
                    "status": status,
                    "last_updated": if km.last_updated.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(km.last_updated.clone())
                    },
                    "auto_update": km.auto_update,
                })
            })
            .collect();

        // ── discover 列表（与面板 load_discover_plugins_from_disk 一致）──
        let mut known = known;
        // 确保 official marketplace 已注册（参考项目行为：自动注入，不落盘）
        let has_official = known.iter().any(|km| match &km.source {
            MarketplaceSource::GitHub { repo } => repo == "anthropics/claude-plugins-official",
            _ => false,
        });
        if !has_official {
            known.push(KnownMarketplace {
                source: MarketplaceSource::GitHub {
                    repo: "anthropics/claude-plugins-official".into(),
                },
                install_location: cache_dir
                    .join("claude-plugins-official")
                    .to_string_lossy()
                    .to_string(),
                auto_update: true,
                last_updated: String::new(),
            });
        }

        let installed_ids: std::collections::HashSet<String> =
            installed.plugins.iter().map(|p| p.id.clone()).collect();
        let mut discover: Vec<serde_json::Value> = Vec::new();
        for km in &known {
            let mp_name = MarketplaceManager::extract_name(&km.source);
            let mp_dir = cache_dir.join(&mp_name);
            let manifest_path = match crate::plugin::marketplace::find_marketplace_json(&mp_dir) {
                Some(path) => path,
                None => continue,
            };
            if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(plugin_list) = manifest.get("plugins").and_then(|v| v.as_array()) {
                        for p in plugin_list {
                            let name = p
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if name.is_empty() {
                                continue;
                            }
                            let plugin_id = format!("{}@{}", name, mp_name);
                            if installed_ids.contains(&plugin_id) {
                                continue;
                            }
                            // author 可能是字符串或 {"name": "..."} 对象
                            let author = p.get("author").and_then(|v| {
                                v.as_str().map(|s| s.to_string()).or_else(|| {
                                    v.get("name")
                                        .and_then(|n| n.as_str())
                                        .map(|s| s.to_string())
                                })
                            });
                            let version = p
                                .get("version")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .unwrap_or("—")
                                .to_string();
                            discover.push(serde_json::json!({
                                "name": name,
                                "version": version,
                                "marketplace": mp_name,
                                "description": p.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                "author": author,
                            }));
                        }
                    }
                }
            }
        }

        serde_json::json!({
            "marketplaces": marketplaces,
            "discover": discover,
        })
    }
}

/// Settings hooks 加载端口实现：包装 `hooks::loader::load_*_settings_hooks`。
#[derive(Debug, Clone, Copy, Default)]
pub struct SettingsHooksLoader;

impl SettingsHooksPort for SettingsHooksLoader {
    fn global(&self) -> Vec<peri_acp_types::hooks::RegisteredHook> {
        crate::hooks::loader::load_global_settings_hooks()
    }

    fn project(&self, cwd: &str) -> Vec<peri_acp_types::hooks::RegisteredHook> {
        crate::hooks::loader::load_settings_project_hooks(cwd)
    }

    fn local(&self, cwd: &str) -> Vec<peri_acp_types::hooks::RegisteredHook> {
        crate::hooks::loader::load_settings_local_hooks(cwd)
    }
}

/// Skills 扫描端口实现：包装 `SkillsMiddleware::resolve_roots_static` /
/// `scan_skill_roots` / `scan_agents_detailed`。
#[derive(Debug, Clone, Copy, Default)]
pub struct SkillsProvider;

impl SkillsPort for SkillsProvider {
    fn available_skills(&self, cwd: &str, plugin_roots: &[SkillRoot]) -> Vec<SkillMetadata> {
        let disable_bundled = crate::skills::load_disable_bundled_skills();
        let skill_roots = crate::SkillsMiddleware::resolve_roots_static(
            cwd,
            plugin_roots.to_vec(),
            disable_bundled,
        );
        crate::skills::scan_skill_roots(&skill_roots)
    }

    fn agents(
        &self,
        cwd: &str,
        extra_dirs: &[PathBuf],
    ) -> Vec<(String, String, String, AgentCapability)> {
        crate::scan_agents_detailed(cwd, extra_dirs)
    }
}
