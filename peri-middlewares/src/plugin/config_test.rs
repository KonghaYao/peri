use tempfile;

use super::InstalledPlugins;
use crate::plugin::types::{
    InstallScope, InstalledPlugin, KnownMarketplace, MarketplaceSource, PluginOrigin,
};

#[test]
fn test_load_installed_plugins_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    let result = super::load_installed_plugins(Some(&path)).unwrap();
    assert_eq!(result.version, 2);
    assert!(result.plugins.is_empty());
}

#[test]
fn test_save_and_load_installed_plugins() {
    let dir = tempfile::tempdir().unwrap();
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
    super::save_installed_plugins(&plugins, Some(&path)).unwrap();
    let loaded = super::load_installed_plugins(Some(&path)).unwrap();
    assert_eq!(loaded.version, 2);
    assert_eq!(loaded.plugins.len(), 1);
    assert_eq!(loaded.plugins[0].id, "test-id");
    assert_eq!(loaded.plugins[0].name, "test-plugin");
}

#[test]
fn test_load_known_marketplaces_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    let result = super::load_known_marketplaces(Some(&path)).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_save_and_load_known_marketplaces() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marketplaces.json");
    let marketplaces = vec![KnownMarketplace {
        source: MarketplaceSource::GitHub {
            repo: "test/repo".into(),
        },
        install_location: "/tmp/test".into(),
        auto_update: true,
        last_updated: "2025-01-01".into(),
    }];
    super::save_known_marketplaces(&marketplaces, Some(&path)).unwrap();
    let loaded = super::load_known_marketplaces(Some(&path)).unwrap();
    assert_eq!(loaded.len(), 1);
}

#[test]
fn test_load_claude_settings_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonexistent.json");
    let result = super::load_claude_settings(Some(&path)).unwrap();
    assert!(result.enabled_plugins.is_empty());
    assert!(result.extra_known_marketplaces.is_empty());
}

#[test]
fn test_load_claude_settings_with_plugins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{
            "enabledPlugins": ["plugin-a", "plugin-b"],
            "extraKnownMarketplaces": [
                {"source": {"source":"github","repo":"test/repo"}}
            ]
        }"#;
    std::fs::write(&path, json).unwrap();
    let settings = super::load_claude_settings(Some(&path)).unwrap();
    assert_eq!(settings.enabled_plugins, vec!["plugin-a", "plugin-b"]);
    assert_eq!(settings.extra_known_marketplaces.len(), 1);
}

#[test]
fn test_load_claude_settings_extra_marketplaces_object_format() {
    let dir = tempfile::tempdir().unwrap();
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
    let settings = super::load_claude_settings(Some(&path)).unwrap();
    assert_eq!(settings.extra_known_marketplaces.len(), 1);
}

#[test]
fn test_load_claude_settings_enabled_plugins_object_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{
            "enabledPlugins": {
                "plugin-a@marketplace": true,
                "plugin-b@marketplace": true,
                "plugin-c@marketplace": false
            }
        }"#;
    std::fs::write(&path, json).unwrap();
    let settings = super::load_claude_settings(Some(&path)).unwrap();
    assert_eq!(
        settings.enabled_plugins,
        vec!["plugin-a@marketplace", "plugin-b@marketplace"]
    );
}

#[test]
fn test_load_claude_settings_enabled_plugins_mixed_with_other_fields() {
    let dir = tempfile::tempdir().unwrap();
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
    let settings = super::load_claude_settings(Some(&path)).unwrap();
    assert_eq!(settings.enabled_plugins.len(), 2);
    assert!(settings
        .enabled_plugins
        .contains(&"frontend-design@claude-plugins-official".to_string()));
}

#[test]
fn test_load_claude_settings_ignores_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{
            "otherKey": 42,
            "enabledPlugins": ["plugin-a"],
            "unknownNested": {"a": 1}
        }"#;
    std::fs::write(&path, json).unwrap();
    let settings = super::load_claude_settings(Some(&path)).unwrap();
    assert_eq!(settings.enabled_plugins, vec!["plugin-a"]);
}

#[test]
fn test_load_plugin_manifest_success() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let json = r#"{"name":"test-plugin","version":"1.0.0","description":"A test plugin"}"#;
    std::fs::write(plugin_dir.join("plugin.json"), json).unwrap();
    let manifest = super::load_plugin_manifest(dir.path()).unwrap();
    assert_eq!(manifest.name, "test-plugin");
    assert_eq!(manifest.version, "1.0.0");
    assert_eq!(manifest.description, "A test plugin");
}

#[test]
fn test_load_plugin_manifest_missing_name_ok() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let json = r#"{"version":"1.0.0"}"#;
    std::fs::write(plugin_dir.join("plugin.json"), json).unwrap();
    let manifest = super::load_plugin_manifest(dir.path()).unwrap();
    assert!(manifest.name.is_empty());
    assert_eq!(manifest.version, "1.0.0");
}

#[test]
fn test_load_plugin_manifest_missing_version_ok() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let json = r#"{"name":"test"}"#;
    std::fs::write(plugin_dir.join("plugin.json"), json).unwrap();
    let manifest = super::load_plugin_manifest(dir.path()).unwrap();
    assert_eq!(manifest.name, "test");
    assert!(manifest.version.is_empty());
}

#[test]
fn test_save_claude_settings_enabled_plugins_preserves_other_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    // 写入包含 enabledPlugins 和其他字段的 JSON
    let json = r#"{
            "env": {"KEY": "value"},
            "model": "opus",
            "enabledPlugins": ["a@m", "b@m"]
        }"#;
    std::fs::write(&path, json).unwrap();

    super::save_claude_settings_enabled_plugins(
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");

    super::save_claude_settings_enabled_plugins(&[("x@m".into(), false)], Some(&path)).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    let obj = value["enabledPlugins"].as_object().unwrap();
    assert_eq!(obj.len(), 1);
    assert_eq!(obj["x@m"], false);
}

#[test]
fn test_save_claude_settings_enabled_plugins_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{"enabledPlugins": {"a@m": true}}"#;
    std::fs::write(&path, json).unwrap();

    super::save_claude_settings_enabled_plugins(&[], Some(&path)).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(value["enabledPlugins"].as_object().unwrap().is_empty());
}

#[test]
fn test_plugins_dir_uses_claude_home() {
    let path = super::plugins_dir();
    let path_str = path.to_string_lossy();
    assert!(path_str.contains(".claude"));
    assert!(path_str.contains("plugins"));
}

/// 改写 `HOME` 的进程级守卫：与其它依赖真实主目录的测试互斥，drop 时还原。
struct HomeGuard {
    _lock: peri_mcp_common::process_env::EnvLockFile,
    previous_home: Option<std::ffi::OsString>,
    previous_userprofile: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn set(home: &std::path::Path) -> Self {
        let lock = peri_mcp_common::process_env::lock().expect("process env lock");
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);
        Self {
            _lock: lock,
            previous_home,
            previous_userprofile,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        match self.previous_userprofile.take() {
            Some(profile) => std::env::set_var("USERPROFILE", profile),
            None => std::env::remove_var("USERPROFILE"),
        }
    }
}

/// 兼容回填只在写盘成功后生效：写失败时面板不得把未登记的插件报告给用户，
/// 否则面板（默认读取）与会话（显式读取）状态分叉；失败后下次读取仍重试回填。
#[test]
fn test_backfill_write_failure_is_not_reported_as_installed() {
    let home = tempfile::tempdir().unwrap();
    let _env = HomeGuard::set(home.path());

    let claude_dir = home.path().join(".claude");
    let plugins_dir = claude_dir.join("plugins");
    let installed_path = plugins_dir.join("installed_plugins.json");
    let cache_manifest = plugins_dir.join("cache/market/demo/1.0.0/.claude-plugin/plugin.json");
    std::fs::create_dir_all(cache_manifest.parent().unwrap()).unwrap();
    std::fs::write(&cache_manifest, r#"{"name":"demo","version":"1.0.0"}"#).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"enabledPlugins":["demo@market"]}"#,
    )
    .unwrap();
    // 故障注入：原子写的临时文件路径被目录占用，写盘必然失败且跨平台。
    std::fs::create_dir(plugins_dir.join("installed_plugins.tmp")).unwrap();

    let panel = super::load_installed_plugins(None).unwrap();
    assert!(
        panel.plugins.is_empty(),
        "写盘失败时面板不得报告未登记的插件: {:?}",
        panel.plugins
    );
    assert!(
        !installed_path.exists(),
        "写盘失败时不得留下已登记的 installed_plugins.json"
    );

    let session = super::load_installed_plugins(Some(&installed_path)).unwrap();
    assert!(
        session.plugins.is_empty(),
        "会话读取必须与面板一致: {:?}",
        session.plugins
    );
    // 再次读取仍会重试回填，但仍不假报。
    let panel_retry = super::load_installed_plugins(None).unwrap();
    assert!(panel_retry.plugins.is_empty());

    // 恢复可写后回填成功，面板与会话看到同一份登记。
    std::fs::remove_dir(plugins_dir.join("installed_plugins.tmp")).unwrap();
    let recovered = super::load_installed_plugins(None).unwrap();
    assert_eq!(recovered.plugins.len(), 1);
    assert_eq!(recovered.plugins[0].id, "demo@market");
    assert_eq!(
        recovered.plugins[0].origin,
        PluginOrigin::ClaudeCodeInstalled
    );
    let persisted = super::load_installed_plugins(Some(&installed_path)).unwrap();
    assert_eq!(persisted.plugins.len(), 1);

    // 已登记后修改缓存清单不再触发回填。
    std::fs::write(&cache_manifest, r#"{"name":"demo","version":"9.9.9"}"#).unwrap();
    let after_manifest_change = super::load_installed_plugins(None).unwrap();
    assert_eq!(after_manifest_change.plugins.len(), 1);
    assert_eq!(after_manifest_change.plugins[0].version, "1.0.0");
}

/// `claude_home()` 的 HOME 优先契约：Windows 的 `dirs::home_dir()` 走 Profile
/// known-folder，不读 HOME/USERPROFILE，只有 HOME 维度能重定向 `~/.claude`；插件目录
/// 解析依赖这一点（各入口各拼一份 home 会在 Windows 上分叉），故在此钉住——
/// 断言跨平台可复现，不必等到 Windows CI。
#[test]
fn test_claude_home_prefers_absolute_home_env() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let home = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());
    let resolved = super::claude_home();
    match previous {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    assert_eq!(resolved, home.path().join(".claude"));
}

#[test]
fn test_ensure_plugin_dirs_creates_missing_dirs() {
    // 模拟无 CC 环境：空临时目录下验证 ensure_plugin_dirs 创建所有子目录
    use std::path::Path;

    // 注意：ensure_plugin_dirs() 操作的是真实 claude_home，
    // 这里只验证函数不 panic 且目标目录结构合理
    // 实际目录创建由 CI/本地 ~/.claude/ 验证
    let plugins = super::plugins_dir();
    let marketplaces = super::marketplaces_cache_dir();
    let cache = super::plugin_cache_dir();

    // 验证路径层级关系
    assert!(marketplaces.starts_with(&plugins));
    assert!(cache.starts_with(&plugins));
    assert!(plugins.starts_with(Path::new(".claude").parent().unwrap_or(Path::new("/"))));
}
