use tempfile::tempdir;
use tokio::sync::mpsc;

use super::{fetch::*, *};
use crate::plugin::types::MarketplacePlugin;

#[test]
fn test_find_marketplace_json_root() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("marketplace.json"), "{}").unwrap();
    let result = find_marketplace_json(dir.path());
    assert!(result.is_some());
    assert_eq!(result.unwrap().file_name().unwrap(), "marketplace.json");
}

#[test]
fn test_find_marketplace_json_subdir() {
    let dir = tempdir().unwrap();
    let subdir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(subdir.join("marketplace.json"), "{}").unwrap();
    let result = find_marketplace_json(dir.path());
    assert!(result.is_some());
}

#[test]
fn test_find_marketplace_json_not_found() {
    let dir = tempdir().unwrap();
    let result = find_marketplace_json(dir.path());
    assert!(result.is_none());
}

#[test]
fn test_find_marketplace_json_priority() {
    let dir = tempdir().unwrap();
    let subdir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::write(dir.path().join("marketplace.json"), "root").unwrap();
    std::fs::write(subdir.join("marketplace.json"), "sub").unwrap();
    let result = find_marketplace_json(dir.path()).unwrap();
    let content = std::fs::read_to_string(result).unwrap();
    assert_eq!(content, "root");
}

#[test]
fn test_read_manifest_from_path_success() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("marketplace.json");
    let json = r#"{"name":"test","plugins":[]}"#;
    std::fs::write(&path, json).unwrap();
    let manifest = read_manifest_from_path(&path).unwrap();
    assert_eq!(manifest.name, "test");
    assert!(manifest.plugins.is_empty());
}

#[test]
fn test_read_manifest_from_path_invalid_json() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("marketplace.json");
    std::fs::write(&path, "not json").unwrap();
    let result = read_manifest_from_path(&path);
    assert!(result.is_err());
    match result.unwrap_err() {
        MarketplaceError::ParseFailed(_) => {}
        _ => panic!("expected ParseFailed"),
    }
}

#[test]
fn test_read_manifest_from_path_not_found() {
    let result = read_manifest_from_path(Path::new("/nonexistent/path.json"));
    assert!(result.is_err());
}

#[test]
fn test_fetch_github_cache_hit() {
    let dir = tempdir().unwrap();
    let cache_base = dir.path().join("marketplaces");
    let cache_dir = cache_base.join("test-repo");
    let plugin_dir = cache_dir.join(".claude-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let json = r#"{"name":"cached-marketplace","plugins":[{"name":"p1","description":"d","source":"s","version":"1.0.0"}]}"#;
    std::fs::write(plugin_dir.join("marketplace.json"), json).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let manifest = rt
        .block_on(fetch_github("test-repo", "some/repo", &cache_base, false))
        .unwrap();
    assert_eq!(manifest.name, "cached-marketplace");
    assert_eq!(manifest.plugins.len(), 1);
}

#[test]
fn test_read_file_success() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("marketplace.json");
    let json = r#"{"name":"file-test","plugins":[]}"#;
    std::fs::write(&path, json).unwrap();
    let manifest = read_file(&path).unwrap();
    assert_eq!(manifest.name, "file-test");
}

#[test]
fn test_read_file_not_found() {
    let result = read_file(Path::new("/nonexistent/file.json"));
    assert!(result.is_err());
}

#[test]
fn test_read_directory_root() {
    let dir = tempdir().unwrap();
    let json = r#"{"name":"dir-test","plugins":[]}"#;
    std::fs::write(dir.path().join("marketplace.json"), json).unwrap();
    let manifest = read_directory(dir.path()).unwrap();
    assert_eq!(manifest.name, "dir-test");
}

#[test]
fn test_read_directory_subdir() {
    let dir = tempdir().unwrap();
    let subdir = dir.path().join(".claude-plugin");
    std::fs::create_dir_all(&subdir).unwrap();
    let json = r#"{"name":"subdir-test","plugins":[]}"#;
    std::fs::write(subdir.join("marketplace.json"), json).unwrap();
    let manifest = read_directory(dir.path()).unwrap();
    assert_eq!(manifest.name, "subdir-test");
}

#[test]
fn test_read_directory_not_found() {
    let dir = tempdir().unwrap();
    let result = read_directory(dir.path());
    assert!(result.is_err());
    match result.unwrap_err() {
        MarketplaceError::ManifestNotFound { .. } => {}
        _ => panic!("expected ManifestNotFound"),
    }
}

/// 拉取失败（连接被拒）⇒ 回退缓存：不需要外部网络，`127.0.0.1:1` 恒无监听。
#[tokio::test]
async fn test_fetch_url_cache_fallback() {
    let dir = tempdir().unwrap();
    let cache_base = dir.path().join("marketplaces");
    std::fs::create_dir_all(&cache_base).unwrap();
    let json = r#"{"name":"cached-url","plugins":[]}"#;
    std::fs::write(cache_base.join("test.json"), json).unwrap();
    let manifest = fetch_url("test", "http://127.0.0.1:1/nonexistent.json", &cache_base)
        .await
        .unwrap();
    assert_eq!(manifest.name, "cached-url");
}

/// 拉取失败且无缓存 ⇒ `HttpFailed`（不静默给出空 manifest）。
#[tokio::test]
async fn test_fetch_url_no_cache_no_server() {
    let dir = tempdir().unwrap();
    let cache_base = dir.path().join("marketplaces");
    std::fs::create_dir_all(&cache_base).unwrap();
    let result = fetch_url("test", "http://127.0.0.1:1/nonexistent.json", &cache_base).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_manager_auto_register_official() {
    let dir = tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(16);
    let mut manager = MarketplaceManager::new(Some(dir.path().to_path_buf()));
    let handles = manager.init(tx).await;

    // Check that official marketplace was registered
    let km_path = dir.path().join("known_marketplaces.json");
    assert!(km_path.exists());
    let known = crate::plugin::config::load_known_marketplaces(Some(&km_path)).unwrap();
    assert!(known.iter().any(|km| match &km.source {
        MarketplaceSource::GitHub { repo } => repo == "anthropics/claude-plugins-official",
        _ => false,
    }));

    for h in handles {
        h.abort();
    }
}

#[tokio::test]
async fn test_manager_merge_extra_known_marketplaces() {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let settings = r#"{
            "extraKnownMarketplaces": [
                {"source": {"source":"file","path":"/test/marketplace.json"}}
            ]
        }"#;
    std::fs::write(&settings_path, settings).unwrap();

    let (tx, _rx) = mpsc::channel(16);
    let mut manager = MarketplaceManager::new(Some(dir.path().to_path_buf()));
    let handles = manager.init(tx).await;

    assert!(manager.entries().iter().any(|e| match &e.source {
        MarketplaceSource::File { path } => path == "/test/marketplace.json",
        _ => false,
    }));

    for h in handles {
        h.abort();
    }
}

#[tokio::test]
async fn test_manager_cache_loading() {
    let dir = tempdir().unwrap();
    let marketplaces_dir = dir.path().join("marketplaces");
    std::fs::create_dir_all(&marketplaces_dir).unwrap();
    let json = r#"{"name":"cached-test","plugins":[{"name":"p1","description":"Plugin 1","source":"s","version":"1.0.0"}]}"#;
    std::fs::write(marketplaces_dir.join("test.json"), json).unwrap();

    let km_path = dir.path().join("known_marketplaces.json");
    // 使用对象格式，包含必需的 installLocation 和 lastUpdated 字段
    let known = r#"{"test": {"source":{"source":"url","url":"https://example.com/test.json"},"installLocation":"","lastUpdated":"2025-01-01T00:00:00Z"}}"#;
    std::fs::write(&km_path, known).unwrap();

    let (tx, _rx) = mpsc::channel(16);
    let mut manager = MarketplaceManager::new(Some(dir.path().to_path_buf()));
    let handles = manager.init(tx).await;

    let cached_entry = manager.entries().iter().find(|e| e.name == "test");
    assert!(cached_entry.is_some());
    let entry = cached_entry.unwrap();
    assert_eq!(entry.status, MarketplaceStatus::Cached);
    assert!(entry.manifest.is_some());

    for h in handles {
        h.abort();
    }
}

#[test]
fn test_manager_find_plugin() {
    let mut manager = MarketplaceManager::new(None);
    let manifest = MarketplaceManifest {
        name: "test-mkt".into(),
        plugins: vec![MarketplacePlugin {
            name: "target-plugin".into(),
            description: "desc".into(),
            source: serde_json::json!("src"),
            version: "1.0.0".into(),
            sha: None,
            author: None,
            category: None,
            homepage: None,
            tags: None,
            extra: serde_json::Value::Object(Default::default()),
        }],
        allow_cross_marketplace: None,
    };
    manager.entries.push(MarketplaceEntry {
        name: "test-mkt".into(),
        source: MarketplaceSource::Directory {
            path: "/tmp/test".into(),
        },
        manifest: Some(manifest),
        status: MarketplaceStatus::Cached,
        last_updated: None,
        auto_update: false,
    });
    let result = manager.find_plugin("target-plugin");
    assert!(result.is_some());
    assert_eq!(result.unwrap().0.name, "target-plugin");
}

#[test]
fn test_manager_find_plugin_not_found() {
    let mut manager = MarketplaceManager::new(None);
    let manifest = MarketplaceManifest {
        name: "test-mkt".into(),
        plugins: vec![],
        allow_cross_marketplace: None,
    };
    manager.entries.push(MarketplaceEntry {
        name: "test-mkt".into(),
        source: MarketplaceSource::Directory {
            path: "/tmp/test".into(),
        },
        manifest: Some(manifest),
        status: MarketplaceStatus::Cached,
        last_updated: None,
        auto_update: false,
    });
    assert!(manager.find_plugin("nonexistent").is_none());
}

#[test]
fn test_manager_available_plugins() {
    let mut manager = MarketplaceManager::new(None);
    let manifest1 = MarketplaceManifest {
        name: "mkt1".into(),
        plugins: vec![
            MarketplacePlugin {
                name: "p1".into(),
                description: "d1".into(),
                source: serde_json::json!("s1"),
                version: "1.0.0".into(),
                sha: None,
                author: None,
                category: None,
                homepage: None,
                tags: None,
                extra: serde_json::Value::Object(Default::default()),
            },
            MarketplacePlugin {
                name: "p2".into(),
                description: "d2".into(),
                source: serde_json::json!("s2"),
                version: "2.0.0".into(),
                sha: None,
                author: None,
                category: None,
                homepage: None,
                tags: None,
                extra: serde_json::Value::Object(Default::default()),
            },
        ],
        allow_cross_marketplace: None,
    };
    manager.entries.push(MarketplaceEntry {
        name: "mkt1".into(),
        source: MarketplaceSource::Directory { path: "/t".into() },
        manifest: Some(manifest1),
        status: MarketplaceStatus::Fresh,
        last_updated: None,
        auto_update: false,
    });
    // NotFetched entry should be skipped
    manager.entries.push(MarketplaceEntry {
        name: "mkt2".into(),
        source: MarketplaceSource::Directory { path: "/t2".into() },
        manifest: None,
        status: MarketplaceStatus::NotFetched,
        last_updated: None,
        auto_update: false,
    });

    let available = manager.available_plugins();
    assert_eq!(available.len(), 2);
    assert_eq!(available[0].name, "p1");
    assert_eq!(available[1].name, "p2");
}

#[test]
fn test_manager_update_entry() {
    let mut manager = MarketplaceManager::new(None);
    manager.entries.push(MarketplaceEntry {
        name: "test".into(),
        source: MarketplaceSource::Directory { path: "/t".into() },
        manifest: None,
        status: MarketplaceStatus::NotFetched,
        last_updated: None,
        auto_update: false,
    });
    let manifest = MarketplaceManifest {
        name: "updated".into(),
        plugins: vec![],
        allow_cross_marketplace: None,
    };
    manager.update_entry(0, manifest, MarketplaceStatus::Fresh);
    assert_eq!(manager.entries[0].status, MarketplaceStatus::Fresh);
    assert!(manager.entries[0].manifest.is_some());
    assert!(manager.entries[0].last_updated.is_some());
}

// ─── parse_marketplace_input tests ───────────────────────────────────

#[test]
fn test_parse_input_empty() {
    assert!(parse_marketplace_input("").is_err());
    assert!(parse_marketplace_input("  ").is_err());
}

#[test]
fn test_parse_input_github_shorthand() {
    let result = parse_marketplace_input("owner/repo").unwrap();
    assert!(matches!(result, MarketplaceSource::GitHub { ref repo } if repo == "owner/repo"));
}

#[test]
fn test_parse_input_github_url() {
    let result = parse_marketplace_input("https://github.com/owner/repo").unwrap();
    assert!(matches!(result, MarketplaceSource::GitHub { ref repo } if repo == "owner/repo"));

    let result2 = parse_marketplace_input("https://github.com/owner/repo.git").unwrap();
    assert!(matches!(result2, MarketplaceSource::GitHub { ref repo } if repo == "owner/repo"));
}

#[test]
fn test_parse_input_ssh_url() {
    let result = parse_marketplace_input("git@github.com:owner/repo.git").unwrap();
    assert!(matches!(result, MarketplaceSource::GitHub { .. }));
}

#[test]
fn test_parse_input_http_url() {
    let result = parse_marketplace_input("https://example.com/marketplace.json").unwrap();
    assert!(
        matches!(result, MarketplaceSource::Url { ref url } if url == "https://example.com/marketplace.json")
    );
}

#[test]
fn test_parse_input_local_directory() {
    let result = parse_marketplace_input("./path/to/marketplace").unwrap();
    assert!(matches!(result, MarketplaceSource::Directory { .. }));
}

#[test]
fn test_parse_input_local_file() {
    let result = parse_marketplace_input("./path/to/marketplace.json").unwrap();
    assert!(matches!(result, MarketplaceSource::File { .. }));
}

#[test]
fn test_parse_input_npm_scoped() {
    let result = parse_marketplace_input("@scope/my-plugin").unwrap();
    assert!(
        matches!(result, MarketplaceSource::Npm { ref package } if package == "@scope/my-plugin")
    );
}

#[test]
fn test_parse_input_npm_unscoped() {
    let result = parse_marketplace_input("my-plugin").unwrap();
    assert!(matches!(result, MarketplaceSource::Npm { ref package } if package == "my-plugin"));
}

#[test]
fn test_parse_input_absolute_path() {
    let result = parse_marketplace_input("/absolute/path/to/dir").unwrap();
    assert!(matches!(result, MarketplaceSource::Directory { .. }));
}
