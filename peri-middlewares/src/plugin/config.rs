use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::plugin::types::{
    DeclaredMarketplace, InstalledPlugins, KnownMarketplace, PluginManifest,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaudeSettings {
    #[serde(default, deserialize_with = "deserialize_enabled_plugins")]
    #[serde(rename = "enabledPlugins")]
    pub enabled_plugins: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_known_marketplaces")]
    #[serde(rename = "extraKnownMarketplaces")]
    pub extra_known_marketplaces: Vec<DeclaredMarketplace>,
}

/// 兼容 Claude Code 两种 enabledPlugins 格式：
/// - 数组: `["plugin-a", "plugin-b"]`
/// - 对象: `{"plugin-a": true, "plugin-b": true}`
fn deserialize_enabled_plugins<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(arr) => {
            let ids: Vec<String> = arr
                .into_iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            Ok(ids)
        }
        serde_json::Value::Object(map) => {
            let ids: Vec<String> = map
                .into_iter()
                .filter_map(|(k, v)| {
                    if matches!(v, serde_json::Value::Bool(true)) {
                        Some(k)
                    } else {
                        None
                    }
                })
                .collect();
            Ok(ids)
        }
        _ => Ok(Vec::new()),
    }
}

/// 兼容 Claude Code 两种 extraKnownMarketplaces 格式：
/// - 数组: `[{source, installLocation, ...}]`
/// - 对象: `{"marketplace-name": {source, installLocation, ...}}`
fn deserialize_known_marketplaces<'de, D>(
    deserializer: D,
) -> Result<Vec<DeclaredMarketplace>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(arr) => {
            serde_json::from_value(serde_json::Value::Array(arr)).map_err(serde::de::Error::custom)
        }
        serde_json::Value::Object(map) => {
            let mut result = Vec::new();
            for (_name, entry) in map {
                match serde_json::from_value::<DeclaredMarketplace>(entry) {
                    Ok(mkt) => result.push(mkt),
                    Err(e) => {
                        tracing::warn!(error = %e, "extraKnownMarketplaces 条目解析失败，跳过");
                    }
                }
            }
            Ok(result)
        }
        _ => Ok(Vec::new()),
    }
}

#[derive(Debug, Error)]
pub enum PluginConfigError {
    #[error("插件配置文件解析失败: {path}: {source}")]
    ParseError {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("插件配置文件读取失败: {path}: {source}")]
    ReadError {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("插件配置文件写入失败: {path}: {source}")]
    WriteError {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// 返回 `~/.claude/` 根目录，不存在时返回 fallback（当前目录）
pub fn claude_home() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
}

/// 返回 `~/.claude/plugins/` 目录
pub fn plugins_dir() -> PathBuf {
    claude_home().join("plugins")
}

/// 返回 `~/.claude/plugins/known_marketplaces.json` 路径
pub fn known_marketplaces_path() -> PathBuf {
    plugins_dir().join("known_marketplaces.json")
}

/// 返回 `~/.claude/plugins/installed_plugins.json` 路径
pub fn installed_plugins_path() -> PathBuf {
    plugins_dir().join("installed_plugins.json")
}

/// 返回 `~/.claude/plugins/marketplaces/` 缓存目录
pub fn marketplaces_cache_dir() -> PathBuf {
    plugins_dir().join("marketplaces")
}

/// 返回 `~/.claude/plugins/cache/` 插件版本缓存目录
pub fn plugin_cache_dir() -> PathBuf {
    plugins_dir().join("cache")
}

/// 返回 `~/.claude/settings.json` 路径
pub fn claude_settings_path() -> PathBuf {
    claude_home().join("settings.json")
}

/// 确保插件系统所需的所有子目录存在（无 CC 环境下的首次启动保障）
pub fn ensure_plugin_dirs() {
    let dirs = [plugins_dir(), marketplaces_cache_dir(), plugin_cache_dir()];
    for dir in dirs {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), error = %e, "创建插件目录失败");
        }
    }
}

fn atomic_write_json(path: &Path, data: &serde_json::Value) -> Result<(), PluginConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| PluginConfigError::WriteError {
            path: path.display().to_string(),
            source: e,
        })?;
    }
    let tmp_path = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(data).map_err(|e| PluginConfigError::ParseError {
        path: path.display().to_string(),
        source: e,
    })?;
    std::fs::write(&tmp_path, &json).map_err(|e| PluginConfigError::WriteError {
        path: tmp_path.display().to_string(),
        source: e,
    })?;
    std::fs::rename(&tmp_path, path).map_err(|e| PluginConfigError::WriteError {
        path: path.display().to_string(),
        source: e,
    })?;
    Ok(())
}

pub fn load_installed_plugins(
    override_path: Option<&Path>,
) -> Result<InstalledPlugins, PluginConfigError> {
    let path = override_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(installed_plugins_path);

    // 如果文件不存在，返回默认值并尝试迁移
    let mut result = if !path.exists() {
        InstalledPlugins::default()
    } else {
        let content = std::fs::read_to_string(&path).map_err(|e| PluginConfigError::ReadError {
            path: path.display().to_string(),
            source: e,
        })?;
        serde_json::from_str(&content).map_err(|e| PluginConfigError::ParseError {
            path: path.display().to_string(),
            source: e,
        })?
    };

    // 迁移：从 settings.json 的 enabledPlugins 回填未记录的插件
    // （用于兼容 Claude Code CLI 安装的插件）。
    // 注意：仅在非测试环境（override_path 为 None）时执行迁移，避免测试读取用户真实配置。
    if override_path.is_none() {
        // settings.json 在 claude_home 目录，不是 plugins 目录
        let settings_path = claude_home().join("settings.json");
        if settings_path.exists() {
            if let Ok(settings_content) = std::fs::read_to_string(&settings_path) {
                if let Ok(settings_value) =
                    serde_json::from_str::<serde_json::Value>(&settings_content)
                {
                    if let Some(enabled) = settings_value.get("enabledPlugins") {
                        let enabled_ids: Vec<String> = match enabled {
                            serde_json::Value::Array(arr) => arr
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect(),
                            serde_json::Value::Object(obj) => obj
                                .iter()
                                .filter(|(_, v)| v.as_bool().unwrap_or(false))
                                .map(|(k, _)| k.clone())
                                .collect(),
                            _ => Vec::new(),
                        };

                        // 收集已记录的插件 ID
                        let recorded_ids: std::collections::HashSet<&str> =
                            result.plugins.iter().map(|p| p.id.as_str()).collect();

                        // 只回填已启用但未记录的插件
                        let missing_ids: Vec<&String> = enabled_ids
                            .iter()
                            .filter(|id| !recorded_ids.contains(id.as_str()))
                            .collect();

                        if !missing_ids.is_empty() {
                            // 创建回填条目（从缓存目录查找实际安装路径）
                            use crate::plugin::types::{
                                InstallScope, InstalledPlugin, PluginOrigin,
                            };
                            let plugins_cache = plugin_cache_dir();
                            let mut migrated_plugins = Vec::new();

                            for plugin_id in &missing_ids {
                                if let Some((name, marketplace)) = plugin_id.split_once('@') {
                                    // 扫描插件缓存目录，找到实际的插件路径
                                    let plugin_base = plugins_cache.join(marketplace).join(name);

                                    // 尝试找到第一个有效的插件目录
                                    let mut found_version = None;
                                    let mut found_install_path = None;

                                    if let Ok(entries) = std::fs::read_dir(&plugin_base) {
                                        for entry in entries.flatten() {
                                            if let Ok(ft) = entry.file_type() {
                                                if ft.is_dir() {
                                                    let version_dir = entry.path();
                                                    let plugin_json = version_dir
                                                        .join(".claude-plugin")
                                                        .join("plugin.json");

                                                    // 检查 plugin.json 是否存在
                                                    if plugin_json.exists() {
                                                        if let Ok(content) =
                                                            std::fs::read_to_string(&plugin_json)
                                                        {
                                                            if let Ok(json) = serde_json::from_str::<
                                                                serde_json::Value,
                                                            >(
                                                                &content
                                                            ) {
                                                                if json
                                                                    .get("name")
                                                                    .and_then(|v| v.as_str())
                                                                    == Some(name)
                                                                {
                                                                    let version = json
                                                                        .get("version")
                                                                        .and_then(|v| v.as_str())
                                                                        .unwrap_or("unknown")
                                                                        .to_string();
                                                                    found_version =
                                                                        Some(version.clone());
                                                                    found_install_path =
                                                                        Some(version_dir);
                                                                    break;
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    if let (Some(version), Some(install_path)) =
                                        (found_version, found_install_path)
                                    {
                                        migrated_plugins.push(InstalledPlugin {
                                            id: (*plugin_id).clone(),
                                            name: name.to_string(),
                                            version,
                                            marketplace: marketplace.to_string(),
                                            install_path,
                                            scope: InstallScope::User,
                                            project_path: None,
                                            origin: PluginOrigin::ClaudeCodeInstalled,
                                        });
                                    }
                                }
                            }

                            if !migrated_plugins.is_empty() {
                                // 将回填的插件添加到现有列表
                                result.plugins.extend(migrated_plugins);
                                // 保存更新后的数据
                                let _ = save_installed_plugins(&result, Some(&path));
                                return Ok(result);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(result)
}

pub fn save_installed_plugins(
    plugins: &InstalledPlugins,
    override_path: Option<&Path>,
) -> Result<(), PluginConfigError> {
    let path = override_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(installed_plugins_path);
    let value = serde_json::to_value(plugins).map_err(|e| PluginConfigError::ParseError {
        path: path.display().to_string(),
        source: e,
    })?;
    atomic_write_json(&path, &value)
}

pub fn load_known_marketplaces(
    override_path: Option<&Path>,
) -> Result<Vec<KnownMarketplace>, PluginConfigError> {
    let path = override_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(known_marketplaces_path);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path).map_err(|e| PluginConfigError::ReadError {
        path: path.display().to_string(),
        source: e,
    })?;

    // 兼容 Claude Code CLI 的对象格式：{"marketplace-name": {...}}
    // 以及内部数组格式：[{...}]
    let value = serde_json::from_str::<serde_json::Value>(&content).map_err(|e| {
        PluginConfigError::ParseError {
            path: path.display().to_string(),
            source: e,
        }
    })?;

    match value {
        serde_json::Value::Array(arr) => serde_json::from_value(serde_json::Value::Array(arr))
            .map_err(|e| PluginConfigError::ParseError {
                path: path.display().to_string(),
                source: e,
            }),
        serde_json::Value::Object(obj) => {
            // Claude Code CLI 格式：对象键为 marketplace 名称
            let mut result = Vec::new();
            for (_name, entry) in obj {
                if let Ok(mkt) = serde_json::from_value::<KnownMarketplace>(entry) {
                    result.push(mkt);
                }
            }
            Ok(result)
        }
        _ => Ok(Vec::new()),
    }
}

pub fn save_known_marketplaces(
    marketplaces: &[KnownMarketplace],
    override_path: Option<&Path>,
) -> Result<(), PluginConfigError> {
    let path = override_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(known_marketplaces_path);

    // 保存为 Claude Code CLI 对象格式：{"marketplace-name": {...}}
    use crate::plugin::MarketplaceManager;
    let mut obj = serde_json::Map::new();
    for mkt in marketplaces {
        let name = MarketplaceManager::extract_name(&mkt.source);

        // 手动构建 JSON 对象，移除 null 值
        let mut entry = serde_json::Map::new();

        // source
        if let Ok(source_val) = serde_json::to_value(&mkt.source) {
            entry.insert("source".into(), source_val);
        }

        // installLocation (required)
        entry.insert(
            "installLocation".into(),
            serde_json::Value::String(mkt.install_location.clone()),
        );

        // lastUpdated (required)
        entry.insert(
            "lastUpdated".into(),
            serde_json::Value::String(mkt.last_updated.clone()),
        );

        // autoUpdate (optional, 仅当为 true 时写入)
        if mkt.auto_update {
            entry.insert("autoUpdate".into(), serde_json::Value::Bool(true));
        }

        obj.insert(name, serde_json::Value::Object(entry));
    }

    atomic_write_json(&path, &serde_json::Value::Object(obj))
}

/// 仅更新 `~/.claude/settings.json` 中的 `enabledPlugins` 字段，保留文件中其他字段不变。
pub fn save_claude_settings_enabled_plugins(
    plugin_states: &[(String, bool)],
    override_path: Option<&Path>,
) -> Result<(), PluginConfigError> {
    let path = override_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(claude_settings_path);

    // 读取现有文件内容以保留 unknown fields
    let mut value: serde_json::Value = if path.exists() {
        let content = std::fs::read_to_string(&path).map_err(|e| PluginConfigError::ReadError {
            path: path.display().to_string(),
            source: e,
        })?;
        serde_json::from_str(&content).map_err(|e| PluginConfigError::ParseError {
            path: path.display().to_string(),
            source: e,
        })?
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    // 更新 enabledPlugins 字段（Claude Code 要求对象格式: {"plugin-id": true/false, ...}）
    let mut enabled_obj = serde_json::Map::new();
    for (id, enabled) in plugin_states {
        enabled_obj.insert(id.clone(), serde_json::Value::Bool(*enabled));
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "enabledPlugins".to_string(),
            serde_json::Value::Object(enabled_obj),
        );
    }

    atomic_write_json(&path, &value)
}

pub fn load_claude_settings(
    override_path: Option<&Path>,
) -> Result<ClaudeSettings, PluginConfigError> {
    let path = override_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(claude_settings_path);
    if !path.exists() {
        return Ok(ClaudeSettings::default());
    }
    let content = std::fs::read_to_string(&path).map_err(|e| PluginConfigError::ReadError {
        path: path.display().to_string(),
        source: e,
    })?;
    serde_json::from_str(&content).map_err(|e| PluginConfigError::ParseError {
        path: path.display().to_string(),
        source: e,
    })
}

pub fn load_plugin_manifest(plugin_dir: &Path) -> Result<PluginManifest, PluginConfigError> {
    let manifest_path = plugin_dir.join(".claude-plugin").join("plugin.json");
    let content =
        std::fs::read_to_string(&manifest_path).map_err(|e| PluginConfigError::ReadError {
            path: manifest_path.display().to_string(),
            source: e,
        })?;
    let manifest: PluginManifest =
        serde_json::from_str(&content).map_err(|e| PluginConfigError::ParseError {
            path: manifest_path.display().to_string(),
            source: e,
        })?;
    // name 和 version 允许为空——Claude Code 的某些插件清单不包含这些字段，
    // 调用方应使用 installed_plugins.json 中的 name/version 作为 fallback。
    Ok(manifest)
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
