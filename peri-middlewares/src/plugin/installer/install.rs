use std::path::Path;

use super::{
    copy_dir_recursive, generate_synthetic_manifest, get_marketplace_manifest, match_project_path,
    update_enabled_plugins, InstallerError,
};
use crate::plugin::{
    config::{load_installed_plugins, load_plugin_manifest, save_installed_plugins},
    types::{InstallScope, InstalledPlugin, PluginOrigin},
};

pub async fn install_plugin(
    name: &str,
    marketplace: &str,
    scope: InstallScope,
    marketplace_cache_dir: &Path,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<InstalledPlugin, InstallerError> {
    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let mut installed = load_installed_plugins(Some(&plugins_path))?;

    let manifest = get_marketplace_manifest(marketplace, marketplace_cache_dir)?;

    let marketplace_plugin = manifest
        .plugins
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| InstallerError::PluginNotFound {
            name: name.into(),
            marketplace: marketplace.into(),
        })?;

    let source_dir = {
        if let Some(obj) = marketplace_plugin.source.as_object() {
            if obj.get("source").and_then(|v| v.as_str()) == Some("url") {
                let url = obj.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                    InstallerError::SettingsError("URL 源缺少 url 字段".to_string())
                })?;

                let external_cache = claude_dir.join("plugins").join("external").join(name);

                if !external_cache.exists() {
                    tokio::task::spawn_blocking({
                        let url = url.to_string();
                        let cache_dir = external_cache.clone();
                        move || {
                            let _ = std::fs::create_dir_all(&cache_dir);
                            let output = std::process::Command::new("git")
                                .args(["clone", "--depth", "1", &url, cache_dir.to_str().unwrap()])
                                .output();
                            match output {
                                Ok(o) if o.status.success() => Ok(()),
                                Ok(o) => Err(format!(
                                    "git clone 失败: {}",
                                    String::from_utf8_lossy(&o.stderr)
                                )),
                                Err(e) => Err(format!("git clone 执行失败: {e}")),
                            }
                        }
                    })
                    .await
                    .map_err(|e| {
                        InstallerError::SettingsError(format!("spawn_blocking 失败: {e}"))
                    })?
                    .map_err(InstallerError::SettingsError)?;
                }

                external_cache
            } else {
                return Err(InstallerError::SettingsError(
                    "不支持的 source 对象格式".to_string(),
                ));
            }
        } else {
            let raw = marketplace_plugin.source.as_str().unwrap_or(".");
            let normalized: std::path::PathBuf = std::path::Path::new(raw)
                .components()
                .filter(|c| matches!(c, std::path::Component::Normal(_)))
                .collect();
            marketplace_cache_dir.join(marketplace).join(normalized)
        }
    };
    if !source_dir.exists() {
        return Err(InstallerError::PluginNotFound {
            name: name.into(),
            marketplace: marketplace.into(),
        });
    }
    let manifest_path = source_dir.join(".claude-plugin").join("plugin.json");
    let has_native_manifest = if manifest_path.exists() {
        load_plugin_manifest(&source_dir)?;
        true
    } else {
        false
    };

    let version = marketplace_plugin
        .sha
        .as_ref()
        .map(|s| s.chars().take(7).collect())
        .unwrap_or_else(|| {
            let v = marketplace_plugin.version.clone();
            if v.is_empty() {
                chrono::Utc::now().format("%Y%m%d%H%M%S").to_string()
            } else {
                v
            }
        });

    let target_dir = claude_dir
        .join("plugins")
        .join("cache")
        .join(marketplace)
        .join(name)
        .join(&version);

    tokio::task::spawn_blocking({
        let source_dir = source_dir.clone();
        let target_dir = target_dir.clone();
        move || -> Result<(), InstallerError> {
            if target_dir.exists() {
                let _ = std::fs::remove_dir_all(&target_dir);
            }
            std::fs::create_dir_all(&target_dir)?;
            copy_dir_recursive(&source_dir, &target_dir).map_err(|e| {
                InstallerError::CopyFailed {
                    src: source_dir.clone(),
                    dst: target_dir.clone(),
                    source: e,
                }
            })?;

            if !has_native_manifest {
                generate_synthetic_manifest(&target_dir, &marketplace_plugin)?;
            }

            Ok(())
        }
    })
    .await
    .map_err(|e| InstallerError::SettingsError(format!("spawn_blocking 失败: {e}")))??;

    let plugin_id = format!("{name}@{marketplace}");
    let project_path = project_dir.and_then(|p| p.to_str()).map(|s| s.to_string());
    let installed_plugin = InstalledPlugin {
        id: plugin_id.clone(),
        name: name.into(),
        version,
        marketplace: marketplace.into(),
        install_path: target_dir,
        scope,
        project_path,
        origin: PluginOrigin::PeriInstalled,
    };

    installed.plugins.retain(|p| {
        !(p.id == plugin_id && p.scope == scope && match_project_path(&p.project_path, project_dir))
    });
    installed.plugins.push(installed_plugin.clone());
    save_installed_plugins(&installed, Some(&plugins_path))?;

    update_enabled_plugins(&plugin_id, scope, claude_dir, project_dir)?;

    Ok(installed_plugin)
}

pub async fn update_plugin(
    plugin_id: &str,
    marketplace_cache_dir: &Path,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<InstalledPlugin, InstallerError> {
    let (name, marketplace) = plugin_id.split_once('@').unwrap_or((plugin_id, ""));

    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let installed = load_installed_plugins(Some(&plugins_path))?;
    let current = installed
        .plugins
        .iter()
        .find(|p| p.id == plugin_id)
        .ok_or_else(|| InstallerError::PluginNotFound {
            name: name.into(),
            marketplace: marketplace.into(),
        })?;

    let manifest = get_marketplace_manifest(marketplace, marketplace_cache_dir)?;
    let latest = manifest
        .plugins
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| InstallerError::PluginNotFound {
            name: name.into(),
            marketplace: marketplace.into(),
        })?;

    let latest_version = latest
        .sha
        .as_ref()
        .map(|s| s.chars().take(7).collect::<String>())
        .unwrap_or_else(|| latest.version.clone());

    if latest_version == current.version {
        return Ok(current.clone());
    }

    super::uninstall::uninstall_plugin(plugin_id, claude_dir, project_dir).await?;
    install_plugin(
        name,
        marketplace,
        current.scope,
        marketplace_cache_dir,
        claude_dir,
        project_dir,
    )
    .await
}
