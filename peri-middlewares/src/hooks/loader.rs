use std::{fs, path::Path};

use crate::{
    hooks::types::{HooksConfig, RegisteredHook},
    plugin::types::PluginManifest,
};

/// Extract hooks config from a plugin.
///
/// Priority:
/// 1. `hooks/hooks.json` file in plugin install directory
/// 2. `hooks` field in `plugin.json` manifest
pub(crate) fn extract_hooks(manifest: &PluginManifest, install_path: &Path) -> Option<HooksConfig> {
    // Priority 1: hooks/hooks.json file
    let hooks_file = install_path.join("hooks").join("hooks.json");
    if hooks_file.exists() {
        if let Ok(content) = fs::read_to_string(&hooks_file) {
            if let Ok(config) = serde_json::from_str::<HooksConfig>(&content) {
                return Some(config);
            }
        }
    }

    // Priority 2: plugin.json hooks field
    manifest.hooks.clone()
}

/// Load hooks from `~/.claude/settings.json` global `hooks` field.
///
/// Returns a list of `RegisteredHook` with `plugin_name = "settings.json"`.
pub fn load_global_settings_hooks() -> Vec<RegisteredHook> {
    let claude_dir = match dirs_next::home_dir() {
        Some(d) => d.join(".claude"),
        None => {
            tracing::warn!("Cannot determine home directory for global hooks");
            return Vec::new();
        }
    };
    let settings_path = claude_dir.join("settings.json");
    if !settings_path.exists() {
        tracing::warn!("No settings.json at {}", settings_path.display());
        return Vec::new();
    }

    tracing::info!("Reading hooks from {}", settings_path.display());

    let content = match fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to read {}: {}", settings_path.display(), e);
            return Vec::new();
        }
    };

    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse {}: {}", settings_path.display(), e);
            return Vec::new();
        }
    };

    let hooks_value = match value.get("hooks") {
        Some(h) if h.is_object() => h,
        None => {
            tracing::warn!("No 'hooks' field in {}", settings_path.display());
            return Vec::new();
        }
        Some(h) => {
            tracing::warn!(
                "'hooks' field in {} is not an object (type: {})",
                settings_path.display(),
                if h.is_array() {
                    "array"
                } else if h.is_string() {
                    "string"
                } else if h.is_null() {
                    "null"
                } else {
                    "other"
                }
            );
            return Vec::new();
        }
    };

    let hooks_config: HooksConfig = match serde_json::from_value(hooks_value.clone()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Failed to parse hooks config from {}: {}",
                settings_path.display(),
                e
            );
            return Vec::new();
        }
    };

    let mut hooks = Vec::new();
    for (event, rules) in &hooks_config {
        for rule in rules {
            for hook_def in &rule.hooks {
                hooks.push(RegisteredHook {
                    hook: hook_def.clone(),
                    event: event.clone(),
                    matcher: rule
                        .matcher
                        .clone()
                        .or_else(|| hook_def.get_matcher().cloned()),
                    plugin_name: "settings.json".to_string(),
                    plugin_id: "settings.global".to_string(),
                    plugin_root: claude_dir.clone(),
                    plugin_data_dir: claude_dir.clone(),
                    plugin_options: std::collections::HashMap::new(),
                });
            }
        }
    }

    tracing::info!(
        "Loaded {} hooks from ~/.claude/settings.json ({} events)",
        hooks.len(),
        hooks_config.len()
    );

    hooks
}

/// Load hooks from `{cwd}/.claude/settings.local.json` `hooks` field.
///
/// Returns a list of `RegisteredHook` with `plugin_name = "settings.local.json"`.
pub fn load_settings_local_hooks(cwd: &str) -> Vec<RegisteredHook> {
    let settings_path = Path::new(cwd).join(".claude").join("settings.local.json");
    if !settings_path.exists() {
        tracing::debug!("No settings.local.json at {}", settings_path.display());
        return Vec::new();
    }

    let content = match fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to read {}: {}", settings_path.display(), e);
            return Vec::new();
        }
    };

    // Parse the top-level JSON to extract the `hooks` field
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse {}: {}", settings_path.display(), e);
            return Vec::new();
        }
    };

    let hooks_value = match value.get("hooks") {
        Some(h) if h.is_object() => h,
        _ => return Vec::new(),
    };

    let hooks_config: HooksConfig = match serde_json::from_value(hooks_value.clone()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Failed to parse hooks config from {}: {}",
                settings_path.display(),
                e
            );
            return Vec::new();
        }
    };

    let mut hooks = Vec::new();
    for (event, rules) in &hooks_config {
        for rule in rules {
            for hook_def in &rule.hooks {
                hooks.push(RegisteredHook {
                    hook: hook_def.clone(),
                    event: event.clone(),
                    matcher: rule
                        .matcher
                        .clone()
                        .or_else(|| hook_def.get_matcher().cloned()),
                    plugin_name: "settings.local.json".to_string(),
                    plugin_id: "settings.local".to_string(),
                    plugin_root: Path::new(cwd).to_path_buf(),
                    plugin_data_dir: Path::new(cwd).join(".claude"),
                    plugin_options: std::collections::HashMap::new(),
                });
            }
        }
    }

    tracing::info!(
        "Loaded {} hooks from settings.local.json ({} events)",
        hooks.len(),
        hooks_config.len()
    );

    hooks
}

/// 从 `{cwd}/.claude/settings.json` 加载项目级 hooks 配置。
///
/// 返回 `RegisteredHook` 列表，`plugin_name = "project-settings.json"`。
pub fn load_settings_project_hooks(cwd: &str) -> Vec<RegisteredHook> {
    let settings_path = Path::new(cwd).join(".claude").join("settings.json");
    if !settings_path.exists() {
        tracing::debug!("No settings.json at {}", settings_path.display());
        return Vec::new();
    }

    let content = match fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to read {}: {}", settings_path.display(), e);
            return Vec::new();
        }
    };

    // 解析顶层 JSON，提取 `hooks` 字段
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse {}: {}", settings_path.display(), e);
            return Vec::new();
        }
    };

    let hooks_value = match value.get("hooks") {
        Some(h) if h.is_object() => h,
        _ => return Vec::new(),
    };

    let hooks_config: HooksConfig = match serde_json::from_value(hooks_value.clone()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Failed to parse hooks config from {}: {}",
                settings_path.display(),
                e
            );
            return Vec::new();
        }
    };

    let mut hooks = Vec::new();
    for (event, rules) in &hooks_config {
        for rule in rules {
            for hook_def in &rule.hooks {
                hooks.push(RegisteredHook {
                    hook: hook_def.clone(),
                    event: event.clone(),
                    matcher: rule
                        .matcher
                        .clone()
                        .or_else(|| hook_def.get_matcher().cloned()),
                    plugin_name: "project-settings.json".to_string(),
                    plugin_id: "settings.project".to_string(),
                    plugin_root: Path::new(cwd).to_path_buf(),
                    plugin_data_dir: Path::new(cwd).join(".claude"),
                    plugin_options: std::collections::HashMap::new(),
                });
            }
        }
    }

    tracing::info!(
        "Loaded {} hooks from project settings.json ({} events)",
        hooks.len(),
        hooks_config.len()
    );

    hooks
}

#[cfg(test)]
#[path = "loader_test.rs"]
mod tests;
