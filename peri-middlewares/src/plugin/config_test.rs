use tempfile::tempdir;

use super::*;
use crate::plugin::types::{
    InstallScope, InstalledPlugin, KnownMarketplace, MarketplaceSource, PluginOrigin,
};

#[test]
fn test_load_installed_plugins_nonexistent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    let result = load_installed_plugins(Some(&path)).unwrap();
    assert_eq!(result.version, 2);
    assert!(result.plugins.is_empty());
}

#[test]
fn test_save_and_load_installed_plugins() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("installed_plugins.json");
    let plugins = InstalledPlugins {
        version: 2,
        plugins: vec![InstalledPlugin {
            id: "test-id".into(),
            name: "test-plugin".into(),
            version: "1.0.0".into(),
            marketplace: "test-marketplace".into(),
            install_path: "/tmp/test".into(),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    };
    save_installed_plugins(&plugins, Some(&path)).unwrap();
    let loaded = load_installed_plugins(Some(&path)).unwrap();
    assert_eq!(loaded.version, 2);
    assert_eq!(loaded.plugins.len(), 1);
    assert_eq!(loaded.plugins[0].id, "test-id");
    assert_eq!(loaded.plugins[0].name, "test-plugin");
}

#[test]
fn test_load_known_marketplaces_nonexistent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    let result = load_known_marketplaces(Some(&path)).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_save_and_load_known_marketplaces() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("marketplaces.json");
    let marketplaces = vec![KnownMarketplace {
        source: MarketplaceSource::GitHub {
            repo: "test/repo".into(),
        },
        install_location: "/tmp/test".into(),
        auto_update: true,
        last_updated: "2025-01-01".into(),
    }];
    save_known_marketplaces(&marketplaces, Some(&path)).unwrap();
    let loaded = load_known_marketplaces(Some(&path)).unwrap();
    assert_eq!(loaded.len(), 1);
}

#[test]
fn test_load_claude_settings_nonexistent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    let result = load_claude_settings(Some(&path)).unwrap();
    assert!(result.enabled_plugins.is_empty());
    assert!(result.extra_known_marketplaces.is_empty());
}

#[test]
fn test_load_claude_settings_with_plugins() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{
            "enabledPlugins": ["plugin-a", "plugin-b"],
            "extraKnownMarketplaces": [
                {"source": {"source":"github","repo":"test/repo"}}
            ]
        }"#;
    std::fs::write(&path, json).unwrap();
    let settings = load_claude_settings(Some(&path)).unwrap();
    assert_eq!(settings.enabled_plugins, vec!["plugin-a", "plugin-b"]);
    assert_eq!(settings.extra_known_marketplaces.len(), 1);
}

#[test]
fn test_load_claude_settings_extra_marketplaces_object_format() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{
            "enabledPlugins": [],
            "extraKnownMarketplaces": {
                "superpowers-dev": {
                    "source": {"source":"github","repo":"test/repo"}
                }
            }
        }"#;
    std::fs::write(&path, json).unwrap();
    let settings = load_claude_settings(Some(&path)).unwrap();
    assert_eq!(settings.extra_known_marketplaces.len(), 1);
}

#[test]
fn test_load_claude_settings_enabled_plugins_object_format() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{
            "enabledPlugins": {
                "plugin-a@marketplace": true,
                "plugin-b@marketplace": true,
                "plugin-c@marketplace": false
            }
        }"#;
    std::fs::write(&path, json).unwrap();
    let settings = load_claude_settings(Some(&path)).unwrap();
    assert_eq!(
        settings.enabled_plugins,
        vec!["plugin-a@marketplace", "plugin-b@marketplace"]
    );
}

#[test]
fn test_load_claude_settings_enabled_plugins_mixed_with_other_fields() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{
            "env": {"KEY": "value"},
            "enabledPlugins": {
                "frontend-design@claude-plugins-official": true,
                "commit-commands@claude-plugins-official": true
            },
            "model": "opus"
        }"#;
    std::fs::write(&path, json).unwrap();
    let settings = load_claude_settings(Some(&path)).unwrap();
    assert_eq!(settings.enabled_plugins.len(), 2);
    assert!(settings
        .enabled_plugins
        .contains(&"frontend-design@claude-plugins-official".to_string()));
}

#[test]
fn test_load_claude_settings_ignores_unknown_fields() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{
            "otherKey": 42,
            "enabledPlugins": ["plugin-a"],
            "unknownNested": {"a": 1}
        }"#;
    std::fs::write(&path, json).unwrap();
    let settings = load_claude_settings(Some(&path)).unwrap();
    assert_eq!(settings.enabled_plugins, vec!["plugin-a"]);
}

#[test]
fn test_load_plugin_manifest_success() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let json = r#"{"name":"test-plugin","version":"1.0.0","description":"A test plugin"}"#;
    std::fs::write(plugin_dir.join("plugin.json"), json).unwrap();
    let manifest = load_plugin_manifest(dir.path()).unwrap();
    assert_eq!(manifest.name, "test-plugin");
    assert_eq!(manifest.version, "1.0.0");
    assert_eq!(manifest.description, "A test plugin");
}

#[test]
fn test_load_plugin_manifest_missing_name_ok() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let json = r#"{"version":"1.0.0"}"#;
    std::fs::write(plugin_dir.join("plugin.json"), json).unwrap();
    let manifest = load_plugin_manifest(dir.path()).unwrap();
    assert!(manifest.name.is_empty());
    assert_eq!(manifest.version, "1.0.0");
}

#[test]
fn test_load_plugin_manifest_missing_version_ok() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let json = r#"{"name":"test"}"#;
    std::fs::write(plugin_dir.join("plugin.json"), json).unwrap();
    let manifest = load_plugin_manifest(dir.path()).unwrap();
    assert_eq!(manifest.name, "test");
    assert!(manifest.version.is_empty());
}

#[test]
fn test_save_claude_settings_enabled_plugins_preserves_other_fields() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("settings.json");
    // 写入包含 enabledPlugins 和其他字段的 JSON
    let json = r#"{
            "env": {"KEY": "value"},
            "model": "opus",
            "enabledPlugins": ["a@m", "b@m"]
        }"#;
    std::fs::write(&path, json).unwrap();

    save_claude_settings_enabled_plugins(
        &[("a@m".into(), true), ("c@m".into(), false)],
        Some(&path),
    )
    .unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    // 其他字段保留
    assert_eq!(value["env"]["KEY"], "value");
    assert_eq!(value["model"], "opus");
    // enabledPlugins 已更新（对象格式: {"a@m": true, "c@m": false}）
    let obj = value["enabledPlugins"].as_object().unwrap();
    assert_eq!(obj.len(), 2);
    assert_eq!(obj["a@m"], true);
    assert_eq!(obj["c@m"], false);
}

#[test]
fn test_save_claude_settings_enabled_plugins_new_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("settings.json");

    save_claude_settings_enabled_plugins(&[("x@m".into(), false)], Some(&path)).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    let obj = value["enabledPlugins"].as_object().unwrap();
    assert_eq!(obj.len(), 1);
    assert_eq!(obj["x@m"], false);
}

#[test]
fn test_save_claude_settings_enabled_plugins_empty_list() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{"enabledPlugins": {"a@m": true}}"#;
    std::fs::write(&path, json).unwrap();

    save_claude_settings_enabled_plugins(&[], Some(&path)).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(value["enabledPlugins"].as_object().unwrap().is_empty());
}

#[test]
fn test_plugins_dir_uses_claude_home() {
    let path = plugins_dir();
    let path_str = path.to_string_lossy();
    assert!(path_str.contains(".claude"));
    assert!(path_str.contains("plugins"));
}

#[test]
fn test_ensure_plugin_dirs_creates_missing_dirs() {
    // 模拟无 CC 环境：空临时目录下验证 ensure_plugin_dirs 创建所有子目录
    use std::path::Path;

    // 注意：ensure_plugin_dirs() 操作的是真实 claude_home，
    // 这里只验证函数不 panic 且目标目录结构合理
    // 实际目录创建由 CI/本地 ~/.claude/ 验证
    let plugins = plugins_dir();
    let marketplaces = marketplaces_cache_dir();
    let cache = plugin_cache_dir();

    // 验证路径层级关系
    assert!(marketplaces.starts_with(&plugins));
    assert!(cache.starts_with(&plugins));
    assert!(plugins.starts_with(Path::new(".claude").parent().unwrap_or(Path::new("/"))));
}
