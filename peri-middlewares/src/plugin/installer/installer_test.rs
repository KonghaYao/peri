use tempfile::tempdir;

use super::*;
use crate::plugin::PluginOrigin;

fn setup_marketplace_cache(cache_dir: &Path) {
    let mkt_dir = cache_dir.join("test-mkt");
    std::fs::create_dir_all(
        mkt_dir
            .join("plugins")
            .join("test-plugin")
            .join(".claude-plugin"),
    )
    .unwrap();
    let marketplace_json = r#"{
            "name": "test-marketplace",
            "plugins": [
                {
                    "name": "test-plugin",
                    "description": "A test plugin",
                    "source": "plugins/test-plugin",
                    "version": "1.0.0",
                    "sha": "abc1234567890"
                }
            ]
        }"#;
    std::fs::write(mkt_dir.join("marketplace.json"), marketplace_json).unwrap();
    let plugin_json = r#"{"name":"test-plugin","version":"1.0.0","description":"Test"}"#;
    std::fs::write(
        mkt_dir
            .join("plugins")
            .join("test-plugin")
            .join(".claude-plugin")
            .join("plugin.json"),
        plugin_json,
    )
    .unwrap();
    // Add a skill file
    std::fs::create_dir_all(
        mkt_dir
            .join("plugins")
            .join("test-plugin")
            .join("skills")
            .join("test-skill"),
    )
    .unwrap();
    std::fs::write(
        mkt_dir
            .join("plugins")
            .join("test-plugin")
            .join("skills")
            .join("test-skill")
            .join("SKILL.md"),
        "---\nname: test-skill\ndescription: test\n---\nTest content",
    )
    .unwrap();
}

#[tokio::test]
async fn test_install_plugin_success() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    let result = install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(result.id, "test-plugin@test-mkt");
    assert_eq!(result.version, "abc1234");
    assert_eq!(result.marketplace, "test-mkt");

    // Verify installed_plugins.json
    let installed = load_installed_plugins(Some(
        &claude_dir
            .path()
            .join("plugins")
            .join("installed_plugins.json"),
    ))
    .unwrap();
    assert_eq!(installed.plugins.len(), 1);
    assert_eq!(installed.plugins[0].id, "test-plugin@test-mkt");

    // Verify cache directory has plugin files
    let plugin_cache = claude_dir
        .path()
        .join("plugins")
        .join("cache")
        .join("test-mkt")
        .join("test-plugin")
        .join("abc1234");
    assert!(plugin_cache
        .join(".claude-plugin")
        .join("plugin.json")
        .exists());

    // Verify settings.json enabledPlugins (对象格式)
    let settings_path = claude_dir.path().join("settings.json");
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(
        enabled
            .get("test-plugin@test-mkt")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn test_install_plugin_not_found() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    let result = install_plugin(
        "nonexistent",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        InstallerError::PluginNotFound { name, .. } => assert_eq!(name, "nonexistent"),
        _ => panic!("expected PluginNotFound"),
    }
}

#[tokio::test]
async fn test_install_plugin_invalid_manifest() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let mkt_dir = cache_dir.path().join("test-mkt");
    std::fs::create_dir_all(mkt_dir.join("bad-plugin").join(".claude-plugin")).unwrap();
    let marketplace_json = r#"{
            "name": "test",
            "plugins": [{"name": "bad-plugin", "description": "", "source": "bad-plugin", "version": "1.0.0"}]
        }"#;
    std::fs::write(mkt_dir.join("marketplace.json"), marketplace_json).unwrap();
    std::fs::write(
        mkt_dir
            .join("bad-plugin")
            .join(".claude-plugin")
            .join("plugin.json"),
        "invalid json{{{",
    )
    .unwrap();

    let result = install_plugin(
        "bad-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_install_plugin_reinstall() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    let installed = load_installed_plugins(Some(
        &claude_dir
            .path()
            .join("plugins")
            .join("installed_plugins.json"),
    ))
    .unwrap();
    assert_eq!(installed.plugins.len(), 1);
}

#[tokio::test]
async fn test_uninstall_plugin() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    uninstall_plugin("test-plugin@test-mkt", claude_dir.path(), None)
        .await
        .unwrap();

    let installed = load_installed_plugins(Some(
        &claude_dir
            .path()
            .join("plugins")
            .join("installed_plugins.json"),
    ))
    .unwrap();
    assert!(installed.plugins.is_empty());

    // Verify settings.json enabledPlugins removed
    let settings_path = claude_dir.path().join("settings.json");
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert!(!enabled.contains_key("test-plugin@test-mkt"));
}

#[tokio::test]
async fn test_uninstall_plugin_not_found() {
    let claude_dir = tempdir().unwrap();
    let result = uninstall_plugin("nonexistent@test", claude_dir.path(), None).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_update_plugin_same_version() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    let installed = install_plugin(
        "test-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    let result = update_plugin(
        "test-plugin@test-mkt",
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.id, installed.id);
    assert_eq!(result.version, installed.version);
}

#[tokio::test]
async fn test_check_updates() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_cache(cache_dir.path());

    // Install plugin with old version
    let mut installed = InstalledPlugins::default();
    installed.plugins.push(InstalledPlugin {
        id: "test-plugin@test-mkt".into(),
        name: "test-plugin".into(),
        version: "old-version".into(),
        marketplace: "test-mkt".into(),
        install_path: claude_dir.path().join("fake").into(),
        scope: InstallScope::User,
        project_path: None,
        origin: PluginOrigin::PeriInstalled,
    });
    // Add a plugin with no update
    installed.plugins.push(InstalledPlugin {
        id: "other@test-mkt".into(),
        name: "other".into(),
        version: "abc1234".into(),
        marketplace: "test-mkt".into(),
        install_path: claude_dir.path().join("fake2").into(),
        scope: InstallScope::User,
        project_path: None,
        origin: PluginOrigin::PeriInstalled,
    });

    let updates = check_updates(&installed, cache_dir.path()).await;
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].plugin_id, "test-plugin@test-mkt");
    assert_eq!(updates[0].latest_version, "abc1234");
    assert_eq!(updates[0].current_version, "old-version");
}

#[test]
fn test_copy_dir_recursive() {
    let src = tempdir().unwrap();
    let dst = tempdir().unwrap();

    // Create nested structure
    std::fs::create_dir_all(src.path().join("sub").join("deep")).unwrap();
    std::fs::write(src.path().join("file1.txt"), "content1").unwrap();
    std::fs::write(src.path().join("sub").join("file2.txt"), "content2").unwrap();
    std::fs::write(
        src.path().join("sub").join("deep").join("file3.txt"),
        "content3",
    )
    .unwrap();

    // Create .git dir (should be skipped)
    std::fs::create_dir_all(src.path().join(".git").join("objects")).unwrap();
    std::fs::write(src.path().join(".git").join("config"), "gitconfig").unwrap();

    copy_dir_recursive(src.path(), &dst.path().join("copy")).unwrap();

    assert!(dst.path().join("copy").join("file1.txt").exists());
    assert!(dst
        .path()
        .join("copy")
        .join("sub")
        .join("file2.txt")
        .exists());
    assert!(dst
        .path()
        .join("copy")
        .join("sub")
        .join("deep")
        .join("file3.txt")
        .exists());
    assert!(!dst.path().join("copy").join(".git").exists());

    // Verify content
    let content = std::fs::read_to_string(dst.path().join("copy").join("file1.txt")).unwrap();
    assert_eq!(content, "content1");
}

#[test]
fn test_update_enabled_plugins_append() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    update_enabled_plugins("plugin-a", InstallScope::User, claude_dir, None).unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    // 现在写入对象格式
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(
        enabled.get("plugin-a").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn test_update_enabled_plugins_dedup() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    // 写入数组格式的现有文件
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":["plugin-a","plugin-b"]}"#,
    )
    .unwrap();

    update_enabled_plugins("plugin-a", InstallScope::User, claude_dir, None).unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    // 应该转换为对象格式
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 2);
    assert!(enabled.contains_key("plugin-a"));
    assert!(enabled.contains_key("plugin-b"));
}

#[test]
fn test_update_enabled_plugins_object_format() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    // 写入对象格式的现有文件（Claude Code 格式）
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":{"plugin-a":true,"plugin-b":true}}"#,
    )
    .unwrap();

    update_enabled_plugins("plugin-c", InstallScope::User, claude_dir, None).unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 3);
    assert_eq!(
        enabled.get("plugin-c").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn test_remove_from_enabled_plugins_array_format() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":["plugin-a","plugin-b"]}"#,
    )
    .unwrap();

    remove_from_enabled_plugins("plugin-a", &InstallScope::User, claude_dir, None).unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_array().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].as_str(), Some("plugin-b"));
}

#[test]
fn test_remove_from_enabled_plugins_object_format() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();
    let settings_path = claude_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"enabledPlugins":{"plugin-a":true,"plugin-b":true}}"#,
    )
    .unwrap();

    remove_from_enabled_plugins("plugin-a", &InstallScope::User, claude_dir, None).unwrap();

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let enabled = settings["enabledPlugins"].as_object().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(
        enabled.get("plugin-b").and_then(|v| v.as_bool()),
        Some(true)
    );
}

// ── sanitize_plugin_id tests ──

#[test]
fn test_sanitize_plugin_id_basic() {
    assert_eq!(sanitize_plugin_id("my-plugin_v2"), "my-plugin_v2");
}

#[test]
fn test_sanitize_plugin_id_special_chars() {
    assert_eq!(
        sanitize_plugin_id("plugin@marketplace"),
        "plugin-marketplace"
    );
    assert_eq!(sanitize_plugin_id("a.b/c"), "a-b-c");
    assert_eq!(sanitize_plugin_id("hello world"), "hello-world");
}

#[test]
fn test_sanitize_plugin_id_empty() {
    assert_eq!(sanitize_plugin_id(""), "");
}

// ── match_project_path tests ──

#[test]
fn test_match_project_path_both_none() {
    assert!(match_project_path(&None, None));
}

#[test]
fn test_match_project_path_stored_none_given_some() {
    assert!(!match_project_path(&None, Some(Path::new("/project"))));
}

#[test]
fn test_match_project_path_given_none_stored_some() {
    assert!(!match_project_path(&Some("/project".into()), None));
}

#[test]
fn test_match_project_path_exact_match() {
    assert!(match_project_path(
        &Some("/home/user/project".into()),
        Some(Path::new("/home/user/project"))
    ));
}

#[test]
fn test_match_project_path_suffix_match() {
    assert!(match_project_path(
        &Some("/home/user/project".into()),
        Some(Path::new("project"))
    ));
    assert!(match_project_path(
        &Some("project".into()),
        Some(Path::new("/home/user/project"))
    ));
}

#[test]
fn test_match_project_path_no_match() {
    assert!(!match_project_path(
        &Some("/home/user/project-a".into()),
        Some(Path::new("/home/user/project-b"))
    ));
}

// ── cleanup_orphaned_plugins tests ──

#[tokio::test]
async fn test_cleanup_no_cache_dir() {
    let dir = tempdir().unwrap();
    let result = cleanup_orphaned_plugins(dir.path()).await.unwrap();
    assert_eq!(result, 0, "no cache dir should return 0");
}

#[tokio::test]
async fn test_cleanup_removes_old_orphaned() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    // Create cache structure: cache/marketplace/plugin/version/
    let version_dir = claude_dir
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join("my-plugin")
        .join("v1");
    std::fs::create_dir_all(&version_dir).unwrap();

    // Write .orphaned_at with a timestamp 8 days ago (> 7 day threshold)
    let eight_days_ago = chrono::Utc::now() - chrono::Duration::try_days(8).unwrap();
    std::fs::write(
        version_dir.join(".orphaned_at"),
        eight_days_ago.to_rfc3339(),
    )
    .unwrap();
    // Set file modified time to 8 days ago
    let eight_days_ago_time = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_millis(eight_days_ago.timestamp_millis() as u64);
    let file_time = filetime::FileTime::from_system_time(eight_days_ago_time);
    filetime::set_file_mtime(version_dir.join(".orphaned_at"), file_time).unwrap();

    // No installed plugins → empty installed_plugins.json
    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();
    assert_eq!(deleted, 1, "should delete 1 old orphaned version");
    assert!(!version_dir.exists(), "old orphaned dir should be removed");
}

#[tokio::test]
async fn test_cleanup_preserves_recent_orphaned() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    let version_dir = claude_dir
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join("my-plugin")
        .join("v1");
    std::fs::create_dir_all(&version_dir).unwrap();

    // .orphaned_at 1 day ago (< 7 day threshold)
    let one_day_ago = chrono::Utc::now() - chrono::Duration::try_days(1).unwrap();
    std::fs::write(version_dir.join(".orphaned_at"), one_day_ago.to_rfc3339()).unwrap();
    let one_day_ago_time = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_millis(one_day_ago.timestamp_millis() as u64);
    let file_time = filetime::FileTime::from_system_time(one_day_ago_time);
    filetime::set_file_mtime(version_dir.join(".orphaned_at"), file_time).unwrap();

    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();
    assert_eq!(deleted, 0, "recent orphaned should not be deleted");
    assert!(
        version_dir.exists(),
        "recent orphaned dir should still exist"
    );
}

#[tokio::test]
async fn test_cleanup_preserves_installed_version() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    let version_dir = claude_dir
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join("my-plugin")
        .join("v1");
    std::fs::create_dir_all(&version_dir).unwrap();

    // Mark as old orphaned
    let eight_days_ago = chrono::Utc::now() - chrono::Duration::try_days(8).unwrap();
    std::fs::write(
        version_dir.join(".orphaned_at"),
        eight_days_ago.to_rfc3339(),
    )
    .unwrap();
    let eight_days_ago_time = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_millis(eight_days_ago.timestamp_millis() as u64);
    let file_time = filetime::FileTime::from_system_time(eight_days_ago_time);
    filetime::set_file_mtime(version_dir.join(".orphaned_at"), file_time).unwrap();

    // Register as installed → should be preserved
    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![InstalledPlugin {
                id: "my-plugin@mkt".into(),
                name: "my-plugin".into(),
                version: "v1".into(),
                marketplace: "mkt".into(),
                install_path: version_dir.clone(),
                scope: InstallScope::User,
                project_path: None,
                origin: PluginOrigin::PeriInstalled,
            }],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();
    assert_eq!(deleted, 0, "installed version should not be deleted");
    assert!(
        version_dir.exists(),
        "installed version dir should still exist"
    );
    assert!(
        !version_dir.join(".orphaned_at").exists(),
        ".orphaned_at marker should be removed for installed version"
    );
}

#[tokio::test]
async fn test_cleanup_removes_empty_parent_dirs() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    // Structure: cache/mkt/plugin/version/
    let version_dir = claude_dir
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join("my-plugin")
        .join("v1");
    std::fs::create_dir_all(&version_dir).unwrap();

    let eight_days_ago = chrono::Utc::now() - chrono::Duration::try_days(8).unwrap();
    std::fs::write(
        version_dir.join(".orphaned_at"),
        eight_days_ago.to_rfc3339(),
    )
    .unwrap();
    let eight_days_ago_time = std::time::SystemTime::UNIX_EPOCH
        + std::time::Duration::from_millis(eight_days_ago.timestamp_millis() as u64);
    let file_time = filetime::FileTime::from_system_time(eight_days_ago_time);
    filetime::set_file_mtime(version_dir.join(".orphaned_at"), file_time).unwrap();

    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let _deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();

    let plugin_dir = claude_dir
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join("my-plugin");
    let mkt_dir = claude_dir.join("plugins").join("cache").join("mkt");
    assert!(!plugin_dir.exists(), "empty plugin dir should be removed");
    assert!(!mkt_dir.exists(), "empty marketplace dir should be removed");
}

#[tokio::test]
async fn test_cleanup_orphaned_no_marker_not_deleted() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path();

    // Version dir without .orphaned_at marker
    let version_dir = claude_dir
        .join("plugins")
        .join("cache")
        .join("mkt")
        .join("my-plugin")
        .join("v1");
    std::fs::create_dir_all(&version_dir).unwrap();
    // Write a dummy file so dir is not empty
    std::fs::write(version_dir.join("plugin.json"), "{}").unwrap();

    let plugins_dir = claude_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    save_installed_plugins(
        &InstalledPlugins {
            version: 1,
            plugins: vec![],
        },
        Some(&plugins_dir.join("installed_plugins.json")),
    )
    .unwrap();

    let deleted = cleanup_orphaned_plugins(claude_dir).await.unwrap();
    assert_eq!(
        deleted, 0,
        "version without orphaned marker should not be deleted"
    );
    assert!(
        version_dir.exists(),
        "version dir without marker should still exist"
    );
}

#[test]
fn test_generate_synthetic_manifest_lsp() {
    let dir = tempdir().unwrap();
    let plugin = crate::plugin::types::MarketplacePlugin {
        name: "rust-analyzer-lsp".into(),
        description: "Rust language server".into(),
        source: serde_json::json!("./plugins/rust-analyzer-lsp"),
        version: "1.0.0".into(),
        sha: None,
        author: None,
        category: None,
        homepage: None,
        tags: None,
        extra: serde_json::json!({
            "lspServers": {
                "rust-analyzer": {
                    "command": "rust-analyzer",
                    "extensionToLanguage": { ".rs": "rust" }
                }
            }
        }),
    };

    generate_synthetic_manifest(dir.path(), &plugin).unwrap();

    let manifest_path = dir.path().join(".claude-plugin").join("plugin.json");
    assert!(manifest_path.exists());

    let content = std::fs::read_to_string(&manifest_path).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(manifest["name"], "rust-analyzer-lsp");
    assert_eq!(manifest["version"], "1.0.0");
    assert_eq!(manifest["description"], "Rust language server");

    let lsp_servers = manifest["lspServers"].as_array().unwrap();
    assert_eq!(lsp_servers.len(), 1);
    assert_eq!(lsp_servers[0]["name"], "rust-analyzer");
    assert_eq!(lsp_servers[0]["command"], "rust-analyzer");
    assert_eq!(lsp_servers[0]["extensionToLanguage"][".rs"], "rust");
}

#[test]
fn test_generate_synthetic_manifest_with_author() {
    let dir = tempdir().unwrap();
    let plugin = crate::plugin::types::MarketplacePlugin {
        name: "test-plugin".into(),
        description: String::new(),
        source: serde_json::json!("."),
        version: "2.0.0".into(),
        sha: None,
        author: Some(crate::plugin::types::PluginAuthor {
            name: "Test".into(),
            url: None,
        }),
        category: None,
        homepage: None,
        tags: None,
        extra: serde_json::Value::Object(Default::default()),
    };

    generate_synthetic_manifest(dir.path(), &plugin).unwrap();

    let content =
        std::fs::read_to_string(dir.path().join(".claude-plugin").join("plugin.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(manifest["author"]["name"], "Test");
    assert!(manifest.get("lspServers").is_none());
}

#[test]
fn test_generate_synthetic_manifest_no_version() {
    let dir = tempdir().unwrap();
    let plugin = crate::plugin::types::MarketplacePlugin {
        name: "minimal".into(),
        description: "desc".into(),
        source: serde_json::json!("."),
        version: String::new(),
        sha: None,
        author: None,
        category: None,
        homepage: None,
        tags: None,
        extra: serde_json::Value::Object(Default::default()),
    };

    generate_synthetic_manifest(dir.path(), &plugin).unwrap();

    let content =
        std::fs::read_to_string(dir.path().join(".claude-plugin").join("plugin.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(manifest["name"], "minimal");
    assert!(manifest.get("version").is_none());
}
