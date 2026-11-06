use tempfile::tempdir;

use super::*;
use crate::plugin::PluginOrigin;

// ── #198：外部清单字段不得逃出插件缓存根 ──────────────────────────────

/// 构造一个 marketplace 与插件源目录；`version` 为 None 时省略该字段。
fn setup_marketplace_plugin_with_version(
    cache_dir: &Path,
    plugin_name: &str,
    version: Option<&str>,
) {
    let mkt_dir = cache_dir.join("test-mkt");
    let source_dir = mkt_dir.join("plugins").join("evil-plugin");
    std::fs::create_dir_all(source_dir.join(".claude-plugin")).unwrap();
    std::fs::write(
        source_dir.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"evil-plugin","version":"1.0.0"}"#,
    )
    .unwrap();
    let mut plugin = serde_json::json!({
        "name": plugin_name,
        "description": "",
        "source": "plugins/evil-plugin",
    });
    if let Some(version) = version {
        plugin["version"] = serde_json::json!(version);
    }
    let manifest = serde_json::json!({
        "name": "test-marketplace",
        "plugins": [plugin]
    });
    std::fs::write(
        mkt_dir.join("marketplace.json"),
        serde_json::to_string(&manifest).unwrap(),
    )
    .unwrap();
}

/// 构造一个版本可控的 marketplace 与插件源目录。
fn setup_marketplace_plugin(cache_dir: &Path, plugin_name: &str, version: &str) {
    setup_marketplace_plugin_with_version(cache_dir, plugin_name, Some(version));
}

fn assert_rejected(result: Result<InstalledPlugin, InstallerError>, field: &str) {
    match result {
        Ok(plugin) => panic!(
            "{field} 非法时必须拒绝安装，实际安装了 {}",
            plugin.install_path.display()
        ),
        Err(InstallerError::SettingsError(message)) => {
            assert!(
                message.contains(field),
                "错误信息应指明字段 {field}: {message}"
            )
        }
        Err(other) => panic!("应返回 SettingsError，实际为 {other:?}"),
    }
}

#[tokio::test]
async fn test_install_plugin_rejects_unsafe_version_segments() {
    for version in ["../../../../sentinel", "..", ".", "nested/1.0.0"] {
        let claude_dir = tempdir().unwrap();
        let cache_dir = tempdir().unwrap();
        setup_marketplace_plugin(cache_dir.path(), "evil-plugin", version);

        // version 为 ../../../../sentinel 时，旧实现会把该目录整个删除再写入。
        // 中间层已存在是旧实现命中 remove_dir_all 的前提（例如插件已装过其他版本）。
        std::fs::create_dir_all(
            claude_dir
                .path()
                .join("plugins")
                .join("cache")
                .join("test-mkt")
                .join("evil-plugin"),
        )
        .unwrap();
        let sentinel = claude_dir.path().join("sentinel");
        std::fs::create_dir_all(&sentinel).unwrap();
        std::fs::write(sentinel.join("keep.txt"), "keep").unwrap();

        let result = install_plugin(
            "evil-plugin",
            "test-mkt",
            InstallScope::User,
            cache_dir.path(),
            claude_dir.path(),
            None,
        )
        .await;

        assert_eq!(
            std::fs::read_to_string(sentinel.join("keep.txt")).unwrap(),
            "keep",
            "version {version:?} 不得删除缓存根之外的目录"
        );
        assert_rejected(result, "版本");
    }
}

#[tokio::test]
async fn test_install_plugin_rejects_absolute_version() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let sentinel = claude_dir.path().join("absolute-sentinel");
    std::fs::create_dir_all(&sentinel).unwrap();
    std::fs::write(sentinel.join("keep.txt"), "keep").unwrap();
    let version = sentinel.to_str().unwrap();
    setup_marketplace_plugin(cache_dir.path(), "evil-plugin", version);

    let result = install_plugin(
        "evil-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert_rejected(result, "版本");
    assert_eq!(
        std::fs::read_to_string(sentinel.join("keep.txt")).unwrap(),
        "keep",
        "绝对路径 version 不得删除任意目录"
    );
}

#[tokio::test]
async fn test_install_plugin_rejects_unsafe_name() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    let name = "../../../sentinel";
    setup_marketplace_plugin(cache_dir.path(), name, "1.0.0");
    // 让 cache/test-mkt 存在，旧实现的 join 才会解析 .. 并命中 remove_dir_all。
    std::fs::create_dir_all(
        claude_dir
            .path()
            .join("plugins")
            .join("cache")
            .join("test-mkt"),
    )
    .unwrap();
    let sentinel_target = claude_dir.path().join("sentinel").join("1.0.0");
    std::fs::create_dir_all(&sentinel_target).unwrap();
    std::fs::write(sentinel_target.join("keep.txt"), "keep").unwrap();

    let result = install_plugin(
        name,
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert_rejected(result, "名称");
    assert_eq!(
        std::fs::read_to_string(sentinel_target.join("keep.txt")).unwrap(),
        "keep",
        "含路径穿越的插件名不得删除缓存根之外的目录"
    );
}

#[tokio::test]
async fn test_install_plugin_rejects_unsafe_marketplace() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();

    let result = install_plugin(
        "evil-plugin",
        "../outside",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert_rejected(result, "marketplace");
}

#[tokio::test]
async fn test_install_plugin_accepts_version_without_sha() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_plugin(cache_dir.path(), "evil-plugin", "1.2.3-beta.1");

    let installed = install_plugin(
        "evil-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(installed.version, "1.2.3-beta.1");
    assert!(claude_dir
        .path()
        .join("plugins/cache/test-mkt/evil-plugin/1.2.3-beta.1/.claude-plugin/plugin.json")
        .exists());
}

#[tokio::test]
async fn test_update_plugin_rejects_unsafe_version_before_uninstall() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_plugin(cache_dir.path(), "evil-plugin", "1.0.0");

    let installed = install_plugin(
        "evil-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();

    setup_marketplace_plugin(cache_dir.path(), "evil-plugin", "../../../../sentinel");
    let sentinel = claude_dir.path().join("sentinel");
    std::fs::create_dir_all(&sentinel).unwrap();
    std::fs::write(sentinel.join("keep.txt"), "keep").unwrap();

    let result = update_plugin(
        "evil-plugin@test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await;

    assert_rejected(result, "版本");
    assert_eq!(
        std::fs::read_to_string(sentinel.join("keep.txt")).unwrap(),
        "keep",
        "更新时也不得删除缓存根之外的目录"
    );

    // 校验必须发生在卸载旧版本之前，原安装记录仍需保留。
    let records = load_installed_plugins(Some(
        &claude_dir
            .path()
            .join("plugins")
            .join("installed_plugins.json"),
    ))
    .unwrap();
    assert_eq!(records.plugins.len(), 1);
    assert_eq!(records.plugins[0].version, "1.0.0");
    assert_eq!(records.plugins[0].install_path, installed.install_path);
}

fn assert_timestamp_version(version: &str) {
    assert!(
        version.len() == 14 && version.bytes().all(|b| b.is_ascii_digit()),
        "缺失/空 version 应回退为 14 位时间戳目录: {version:?}"
    );
}

async fn assert_versionless_install_then_update(version: Option<&str>) {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_plugin_with_version(cache_dir.path(), "evil-plugin", version);

    let installed = install_plugin(
        "evil-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();
    assert_timestamp_version(&installed.version);
    assert!(installed.install_path.is_dir());

    let updated = update_plugin(
        "evil-plugin@test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("version={version:?} 的插件必须可更新: {e}"));
    assert_timestamp_version(&updated.version);
    assert_eq!(updated.id, installed.id);

    let records = load_installed_plugins(Some(
        &claude_dir
            .path()
            .join("plugins")
            .join("installed_plugins.json"),
    ))
    .unwrap();
    assert_eq!(records.plugins.len(), 1);
    assert_eq!(records.plugins[0].version, updated.version);
    assert!(updated.install_path.is_dir());
}

#[tokio::test]
async fn test_missing_version_installs_then_updates() {
    assert_versionless_install_then_update(None).await;
}

#[tokio::test]
async fn test_empty_version_installs_then_updates() {
    assert_versionless_install_then_update(Some("")).await;
}

#[tokio::test]
async fn test_missing_version_update_reinstalls_under_new_timestamp() {
    let claude_dir = tempdir().unwrap();
    let cache_dir = tempdir().unwrap();
    setup_marketplace_plugin_with_version(cache_dir.path(), "evil-plugin", None);

    let installed = install_plugin(
        "evil-plugin",
        "test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();
    assert_timestamp_version(&installed.version);

    // 改成过期版本，确保更新走卸载 + 重装，而不是同版本短路。
    let plugins_path = claude_dir
        .path()
        .join("plugins")
        .join("installed_plugins.json");
    let mut records = load_installed_plugins(Some(&plugins_path)).unwrap();
    records.plugins[0].version = "20000101000000".into();
    save_installed_plugins(&records, Some(&plugins_path)).unwrap();

    let reinstalled = update_plugin(
        "evil-plugin@test-mkt",
        InstallScope::User,
        cache_dir.path(),
        claude_dir.path(),
        None,
    )
    .await
    .unwrap();
    assert_timestamp_version(&reinstalled.version);
    assert_ne!(reinstalled.version, "20000101000000");
    assert!(reinstalled.install_path.is_dir());

    let records = load_installed_plugins(Some(&plugins_path)).unwrap();
    assert_eq!(records.plugins.len(), 1);
    assert_eq!(records.plugins[0].version, reinstalled.version);
    assert_eq!(records.plugins[0].install_path, reinstalled.install_path);
}

#[tokio::test]
async fn test_check_updates_skips_versionless_marketplace_entry() {
    let cache_dir = tempdir().unwrap();
    setup_marketplace_plugin_with_version(cache_dir.path(), "evil-plugin", None);

    let installed = InstalledPlugins {
        version: 1,
        plugins: vec![InstalledPlugin {
            id: "evil-plugin@test-mkt".into(),
            name: "evil-plugin".into(),
            version: "20260101000000".into(),
            marketplace: "test-mkt".into(),
            install_path: cache_dir.path().join("cache"),
            scope: InstallScope::User,
            project_path: None,
            origin: PluginOrigin::PeriInstalled,
        }],
    };

    let updates = check_updates(&installed, cache_dir.path()).await;
    assert!(
        updates.is_empty(),
        "未声明版本的条目无法比较，不应报告更新: {updates:?}"
    );
}
