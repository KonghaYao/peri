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
    install_plugin, load_known_marketplaces, remove_from_enabled_plugins, uninstall_plugin,
    update_enabled_plugins, update_plugin,
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
