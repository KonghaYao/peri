use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use super::{
    atomic_write_settings, get_marketplace_manifest, match_project_path,
    remove_from_enabled_plugins, sanitize_plugin_id, InstallerError, PluginUpdateInfo,
};
use crate::plugin::{
    config::{load_installed_plugins, save_installed_plugins},
    types::InstalledPlugins,
};

pub async fn uninstall_plugin(
    plugin_id: &str,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<(), InstallerError> {
    let (name, marketplace) = plugin_id.split_once('@').unwrap_or((plugin_id, ""));

    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let mut installed = load_installed_plugins(Some(&plugins_path))?;

    let entry = installed
        .plugins
        .iter()
        .find(|p| p.id == plugin_id && match_project_path(&p.project_path, project_dir))
        .ok_or_else(|| InstallerError::PluginNotFound {
            name: name.into(),
            marketplace: marketplace.into(),
        })?;

    let install_path = entry.install_path.clone();
    let scope = entry.scope;
    let is_external = entry.origin.is_external();

    let is_last_scope = !installed.plugins.iter().any(|p| {
        p.id == plugin_id
            && (p.scope != scope
                || (p.scope == scope && !match_project_path(&p.project_path, project_dir)))
    });

    installed.plugins.retain(|p| {
        !(p.id == plugin_id && p.scope == scope && match_project_path(&p.project_path, project_dir))
    });
    save_installed_plugins(&installed, Some(&plugins_path))?;

    remove_from_enabled_plugins(plugin_id, &scope, claude_dir, project_dir)?;

    if is_last_scope {
        // 仅 Peri 安装的插件才执行文件清理；外部插件跳过
        if !is_external {
            let sanitized_id = sanitize_plugin_id(plugin_id);
            let data_dir = claude_dir.join("plugins").join("data").join(&sanitized_id);
            if data_dir.exists() {
                tokio::fs::remove_dir_all(&data_dir).await.ok();
            }

            remove_plugin_options(plugin_id, claude_dir)?;

            let _ = mark_orphaned(&install_path).await;
        }
    }

    Ok(())
}

/// 标记插件版本为孤儿（延迟删除）
async fn mark_orphaned(install_path: &Path) -> Result<(), InstallerError> {
    if !install_path.exists() {
        return Ok(());
    }

    tokio::task::spawn_blocking({
        let path = install_path.to_path_buf();
        move || {
            let orphaned_file = path.join(".orphaned_at");
            let _ = std::fs::write(&orphaned_file, chrono::Utc::now().to_rfc3339());
            Ok::<(), InstallerError>(())
        }
    })
    .await
    .map_err(|e| InstallerError::SettingsError(format!("spawn_blocking 失败: {e}")))?
}

/// 从 settings.json 删除插件配置选项
fn remove_plugin_options(plugin_id: &str, claude_dir: &Path) -> Result<(), InstallerError> {
    let settings_path = claude_dir.join("settings.json");
    if !settings_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&settings_path)?;
    let mut value: serde_json::Value =
        serde_json::from_str(&content).unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    if let Some(obj) = value.as_object_mut() {
        if let Some(configs) = obj.get_mut("pluginConfigs").and_then(|v| v.as_object_mut()) {
            configs.remove(plugin_id);
        }

        atomic_write_settings(&settings_path, &value)?;
    }

    Ok(())
}

pub async fn check_updates(
    installed: &InstalledPlugins,
    marketplace_cache_dir: &Path,
) -> Vec<PluginUpdateInfo> {
    let mut manifest_cache: HashMap<String, crate::plugin::types::MarketplaceManifest> =
        HashMap::new();
    let mut result = Vec::new();

    for plugin in &installed.plugins {
        let (name, marketplace) = plugin.id.split_once('@').unwrap_or((&plugin.id, ""));

        if !manifest_cache.contains_key(marketplace) {
            if let Ok(manifest) = get_marketplace_manifest(marketplace, marketplace_cache_dir) {
                manifest_cache.insert(marketplace.to_string(), manifest);
            } else {
                continue;
            }
        }

        let manifest = &manifest_cache[marketplace];
        if let Some(latest) = manifest.plugins.iter().find(|p| p.name == name) {
            let latest_version = latest
                .sha
                .as_ref()
                .map(|s| s.chars().take(7).collect::<String>())
                .unwrap_or_else(|| latest.version.clone());

            if latest_version != plugin.version {
                result.push(PluginUpdateInfo {
                    plugin_id: plugin.id.clone(),
                    current_version: plugin.version.clone(),
                    latest_version,
                });
            }
        }
    }

    result
}

/// 清理孤儿插件版本（超过 7 天未使用）
pub async fn cleanup_orphaned_plugins(claude_dir: &Path) -> Result<usize, InstallerError> {
    const CLEANUP_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000; // 7 天

    let cache_dir = claude_dir.join("plugins").join("cache");
    if !cache_dir.exists() {
        return Ok(0);
    }

    let installed = load_installed_plugins(Some(
        &claude_dir.join("plugins").join("installed_plugins.json"),
    ))?;
    let installed_paths: std::collections::HashSet<PathBuf> = installed
        .plugins
        .iter()
        .map(|p| p.install_path.clone())
        .collect();

    let now = chrono::Utc::now().timestamp_millis();
    let mut deleted_count = 0;

    let mut entries = tokio::fs::read_dir(&cache_dir)
        .await
        .map_err(|e| InstallerError::SettingsError(format!("读取 cache 目录失败: {e}")))?;

    let mut tasks = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| InstallerError::SettingsError(format!("读取目录条目失败: {e}")))?
    {
        if !entry.file_type().await?.is_dir() {
            continue;
        }

        let marketplace_path = entry.path();
        let installed_paths_clone = installed_paths.clone();

        let task = tokio::task::spawn_blocking(move || {
            let mut count = 0;

            if let Ok(plugin_entries) = std::fs::read_dir(&marketplace_path) {
                for plugin_entry in plugin_entries.flatten() {
                    if !plugin_entry.file_type()?.is_dir() {
                        continue;
                    }

                    let plugin_path = plugin_entry.path();

                    if let Ok(version_entries) = std::fs::read_dir(&plugin_path) {
                        for version_entry in version_entries.flatten() {
                            if !version_entry.file_type()?.is_dir() {
                                continue;
                            }

                            let version_path = version_entry.path();

                            if installed_paths_clone.contains(&version_path) {
                                let _ = std::fs::remove_file(version_path.join(".orphaned_at"));
                                continue;
                            }

                            let orphaned_file = version_path.join(".orphaned_at");
                            if let Ok(metadata) = std::fs::metadata(&orphaned_file) {
                                if let Ok(modified) = metadata.modified() {
                                    let age_ms = now
                                        - modified
                                            .duration_since(std::time::SystemTime::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis()
                                            as i64;

                                    if age_ms > CLEANUP_AGE_MS
                                        && std::fs::remove_dir_all(&version_path).is_ok()
                                    {
                                        count += 1;
                                    }
                                }
                            }
                        }

                        if plugin_path.read_dir()?.count() == 0 {
                            let _ = std::fs::remove_dir(&plugin_path);
                        }
                    }
                }

                if marketplace_path.read_dir()?.count() == 0 {
                    let _ = std::fs::remove_dir(&marketplace_path);
                }
            }

            Ok::<usize, InstallerError>(count)
        });

        tasks.push(task);
    }

    for task in tasks {
        if let Ok(Ok(count)) = task.await {
            deleted_count += count;
        }
    }

    Ok(deleted_count)
}
