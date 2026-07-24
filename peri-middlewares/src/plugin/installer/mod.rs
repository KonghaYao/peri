use std::path::{Path, PathBuf};

use thiserror::Error;

#[cfg(test)]
use crate::plugin::config::{load_installed_plugins, save_installed_plugins};
use crate::plugin::types::InstallScope;
#[cfg(test)]
use crate::plugin::types::{InstalledPlugin, InstalledPlugins};
use crate::plugin::{marketplace::read_manifest_from_path, PluginConfigError};

mod install;
mod uninstall;

pub use install::{install_plugin, update_plugin};
pub use uninstall::{check_updates, cleanup_orphaned_plugins, uninstall_plugin};

// ─── Plugin Discovery ──────────────────────────────────────────────────

/// 在所有已知 marketplace 中查找插件，返回找到的 marketplace 名称。
///
/// 当 `plugin install <name>` 未指定 @marketplace 时使用此函数进行自动发现。
pub fn find_plugin_in_marketplaces(
    plugin_name: &str,
    marketplace_cache_dir: &Path,
) -> Result<String, InstallerError> {
    let known = crate::plugin::config::load_known_marketplaces(None)
        .map_err(|e| InstallerError::SettingsError(e.to_string()))?;

    for mkt in &known {
        let mkt_name = crate::plugin::marketplace::MarketplaceManager::extract_name(&mkt.source);
        match get_marketplace_manifest(&mkt_name, marketplace_cache_dir) {
            Ok(manifest) => {
                if manifest.plugins.iter().any(|p| p.name == plugin_name) {
                    return Ok(mkt_name);
                }
            }
            Err(_) => continue, // 该 marketplace 不可用，跳过
        }
    }
    Err(InstallerError::PluginNotFound {
        name: plugin_name.into(),
        marketplace: "所有已配置的 marketplace".into(),
    })
}

// ─── Error & Types ────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum InstallerError {
    #[error("插件未找到: {name} (marketplace: {marketplace})")]
    PluginNotFound { name: String, marketplace: String },
    #[error("复制失败: {src} -> {dst}")]
    CopyFailed {
        src: PathBuf,
        dst: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("配置错误: {0}")]
    ConfigError(#[from] PluginConfigError),
    #[error("Settings 错误: {0}")]
    SettingsError(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct PluginUpdateInfo {
    pub plugin_id: String,
    pub current_version: String,
    pub latest_version: String,
}

// ─── Utility Functions ────────────────────────────────────────────────

pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_name = entry.file_name();
        if file_name == ".git" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&file_name);
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// 从 marketplace 条目生成合成 plugin.json（用于无原生 manifest 的 LSP/MCP 插件）
pub(crate) fn generate_synthetic_manifest(
    target_dir: &Path,
    marketplace_plugin: &crate::plugin::types::MarketplacePlugin,
) -> Result<(), std::io::Error> {
    let mut manifest = serde_json::Map::new();
    manifest.insert("name".into(), serde_json::json!(marketplace_plugin.name));
    if !marketplace_plugin.version.is_empty() {
        manifest.insert(
            "version".into(),
            serde_json::json!(marketplace_plugin.version),
        );
    }
    if !marketplace_plugin.description.is_empty() {
        manifest.insert(
            "description".into(),
            serde_json::json!(marketplace_plugin.description),
        );
    }
    if let Some(ref author) = marketplace_plugin.author {
        if let Ok(val) = serde_json::to_value(author) {
            manifest.insert("author".into(), val);
        }
    }

    if let Some(lsp_servers) = marketplace_plugin.extra.get("lspServers") {
        if let Some(map) = lsp_servers.as_object() {
            let entries: Vec<serde_json::Value> = map
                .iter()
                .map(|(server_name, config)| {
                    let mut entry = config.clone();
                    if let Some(obj) = entry.as_object_mut() {
                        obj.insert("name".into(), serde_json::json!(server_name));
                    }
                    entry
                })
                .collect();
            if !entries.is_empty() {
                manifest.insert("lspServers".into(), serde_json::json!(entries));
            }
        }
    }

    if let Some(mcp_servers) = marketplace_plugin.extra.get("mcpServers") {
        manifest.insert("mcpServers".into(), mcp_servers.clone());
    }

    let claude_plugin_dir = target_dir.join(".claude-plugin");
    std::fs::create_dir_all(&claude_plugin_dir)?;
    let manifest_path = claude_plugin_dir.join("plugin.json");
    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(&manifest_path, json)?;

    Ok(())
}

pub(crate) fn get_marketplace_manifest(
    marketplace: &str,
    marketplace_cache_dir: &Path,
) -> Result<crate::plugin::types::MarketplaceManifest, InstallerError> {
    let path = marketplace_cache_dir.join(marketplace);
    let root = path.join("marketplace.json");
    let subdir = path.join(".claude-plugin").join("marketplace.json");
    let manifest_path = if root.exists() {
        root
    } else if subdir.exists() {
        subdir
    } else {
        return Err(InstallerError::PluginNotFound {
            name: String::new(),
            marketplace: marketplace.into(),
        });
    };
    read_manifest_from_path(&manifest_path)
        .map_err(|e| InstallerError::SettingsError(e.to_string()))
}

pub(crate) fn atomic_write_settings(
    path: &Path,
    value: &serde_json::Value,
) -> Result<(), InstallerError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| InstallerError::SettingsError(e.to_string()))?;
    let tmp_path = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp_path, &json)?;
    std::fs::rename(&tmp_path, path)
        .map_err(|e| InstallerError::SettingsError(format!("rename 失败: {e}")))?;
    Ok(())
}

pub fn update_enabled_plugins(
    plugin_id: &str,
    scope: InstallScope,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<(), InstallerError> {
    let settings_path = match scope {
        InstallScope::User => claude_dir.join("settings.json"),
        InstallScope::Project => {
            if let Some(pd) = project_dir {
                pd.join(".claude").join("settings.json")
            } else {
                claude_dir.join("settings.json")
            }
        }
        InstallScope::Local => claude_dir.join("settings.json"),
    };

    let mut value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    let obj = value.as_object_mut().unwrap();
    let enabled = obj
        .entry("enabledPlugins")
        .or_insert(serde_json::Value::Object(serde_json::Map::new()));

    let enabled_map = if let Some(arr) = enabled.as_array() {
        let map: serde_json::Map<String, serde_json::Value> = arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| (s.to_string(), serde_json::Value::Bool(true)))
            .collect();
        *enabled = serde_json::Value::Object(map.clone());
        map
    } else {
        enabled.as_object().cloned().unwrap_or_default()
    };

    if !enabled_map.contains_key(plugin_id) {
        if let Some(obj) = enabled.as_object_mut() {
            obj.insert(plugin_id.to_string(), serde_json::Value::Bool(true));
        }
    }

    atomic_write_settings(&settings_path, &value)
}

pub fn remove_from_enabled_plugins(
    plugin_id: &str,
    scope: &InstallScope,
    claude_dir: &Path,
    project_dir: Option<&Path>,
) -> Result<(), InstallerError> {
    let settings_path = match scope {
        InstallScope::User => claude_dir.join("settings.json"),
        InstallScope::Project => {
            if let Some(pd) = project_dir {
                pd.join(".claude").join("settings.json")
            } else {
                claude_dir.join("settings.json")
            }
        }
        InstallScope::Local => claude_dir.join("settings.json"),
    };

    if !settings_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&settings_path)?;
    let mut value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| InstallerError::SettingsError(e.to_string()))?;

    if let Some(obj) = value.as_object_mut() {
        if let Some(enabled) = obj.get_mut("enabledPlugins") {
            if let Some(arr) = enabled.as_array_mut() {
                arr.retain(|v| v.as_str() != Some(plugin_id));
            } else if let Some(map) = enabled.as_object_mut() {
                map.remove(plugin_id);
            }
        }
    }

    atomic_write_settings(&settings_path, &value)
}

/// 匹配 project_path：两者都为 None，或者路径字符串匹配
pub(crate) fn match_project_path(stored: &Option<String>, given: Option<&Path>) -> bool {
    match (stored, given) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(_), None) => false,
        (Some(s), Some(p)) => {
            let given_str = p.to_str().unwrap_or("");
            s == given_str || s.ends_with(given_str) || given_str.ends_with(s)
        }
    }
}

/// 清理插件 ID 中的特殊字符，用于目录名
pub(crate) fn sanitize_plugin_id(plugin_id: &str) -> String {
    plugin_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "installer_test.rs"]
mod tests;
