use super::*;

async fn marketplace_config(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let config = make_peri_config_with_provider(make_provider_config(
        "fixture",
        "openai",
        "fixture-unused",
        "fixture-model",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    make_server_config(config, provider, tmp).await
}

async fn request(cfg: &AcpServerConfig, method: &str, params: Value) -> Result<Value, AcpError> {
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    handle_request(method, &params, cfg, &mut HashMap::new(), &transport).await
}

fn write_catalog(tmp: &tempfile::TempDir) -> PathBuf {
    let catalog = tmp.path().join("local-catalog");
    std::fs::create_dir_all(&catalog).unwrap();
    std::fs::write(
        catalog.join("marketplace.json"),
        r#"{"name":"local-catalog","plugins":[]}"#,
    )
    .unwrap();
    catalog
}

#[tokio::test]
#[serial]
async fn marketplace_add_refresh_remove_persist_and_preserve_local_source() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cfg = marketplace_config(&tmp).await;
    let catalog = write_catalog(&tmp);
    let added = request(
        &cfg,
        "marketplace/add",
        json!({"source": catalog, "sessionId": "ticket-context-only"}),
    )
    .await
    .unwrap();
    assert_eq!(added, json!({"success": true, "name": "local-catalog"}));
    let known = peri_middlewares::plugin::load_known_marketplaces(None).unwrap();
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].install_location, catalog.to_string_lossy());
    let refreshed = request(
        &cfg,
        "marketplace/refresh",
        json!({"name": "local-catalog"}),
    )
    .await
    .unwrap();
    assert_eq!(refreshed, json!({"success": true, "pluginCount": 0}));
    let removed = request(&cfg, "marketplace/remove", json!({"name": "local-catalog"}))
        .await
        .unwrap();
    assert_eq!(removed, json!({"success": true}));
    assert!(peri_middlewares::plugin::load_known_marketplaces(None)
        .unwrap()
        .is_empty());
    assert!(catalog.join("marketplace.json").exists());
}

#[tokio::test]
#[serial]
async fn marketplace_mutations_propagate_read_failure_without_overwriting_config() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cfg = marketplace_config(&tmp).await;
    let catalog = write_catalog(&tmp);
    let path = peri_middlewares::plugin::known_marketplaces_path();
    std::fs::create_dir_all(&path).unwrap();
    for (method, params) in [
        ("marketplace/add", json!({"source": catalog})),
        ("marketplace/remove", json!({"name": "local-catalog"})),
        ("marketplace/refresh", json!({"name": "local-catalog"})),
    ] {
        let error = request(&cfg, method, params).await.unwrap_err();
        assert_eq!(error.code, -32603);
        assert!(error.message.contains("known_marketplaces.json"));
        assert!(path.is_dir());
    }
}

#[tokio::test]
#[serial]
async fn marketplace_mutations_propagate_write_failure_and_preserve_saved_config() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cfg = marketplace_config(&tmp).await;
    let catalog = write_catalog(&tmp);
    request(&cfg, "marketplace/add", json!({"source": catalog}))
        .await
        .unwrap();
    let path = peri_middlewares::plugin::known_marketplaces_path();
    let before = std::fs::read(&path).unwrap();
    std::fs::create_dir(path.with_extension("tmp")).unwrap();
    let second = tmp.path().join("another-catalog");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::copy(
        catalog.join("marketplace.json"),
        second.join("marketplace.json"),
    )
    .unwrap();
    for (method, params) in [
        ("marketplace/add", json!({"source": second})),
        ("marketplace/remove", json!({"name": "local-catalog"})),
    ] {
        let error = request(&cfg, method, params).await.unwrap_err();
        assert_eq!(error.code, -32603);
        assert!(error.message.contains("known_marketplaces.tmp"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(catalog.exists());
    }
}

#[tokio::test]
async fn marketplace_mutation_requests_validate_required_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = marketplace_config(&tmp).await;
    for method in [
        "marketplace/add",
        "marketplace/remove",
        "marketplace/refresh",
    ] {
        assert_eq!(
            request(&cfg, method, json!({})).await.unwrap_err().code,
            -32602
        );
    }
}

#[tokio::test]
#[serial]
async fn marketplace_remove_cleans_only_canonical_strict_cache_descendants() {
    use peri_middlewares::plugin::{save_known_marketplaces, KnownMarketplace, MarketplaceSource};

    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cfg = marketplace_config(&tmp).await;
    let cache = cfg.plugin_manager.cache_dir();
    std::fs::create_dir_all(&cache).unwrap();
    let outside = cache.parent().unwrap().join("outside");
    let cached = cache.join("owned-catalog");
    let local = cache.join("local-source");
    for directory in [&outside, &cached, &local] {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join("keep"), "source data").unwrap();
    }
    let mut cases = vec![
        (cache.join("../outside"), false, false),
        (cache.clone(), false, false),
        (local.clone(), true, false),
        (cached.clone(), false, true),
    ];
    #[cfg(unix)]
    {
        let link = cache.join("external-link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        cases.push((link, false, false));
    }
    for (location, local_source, should_remove) in cases {
        let source = if local_source {
            MarketplaceSource::Directory {
                path: local.to_string_lossy().into_owned(),
            }
        } else {
            MarketplaceSource::GitHub {
                repo: "owner/catalog".into(),
            }
        };
        let name = peri_middlewares::plugin::MarketplaceManager::extract_name(&source);
        save_known_marketplaces(
            &[KnownMarketplace {
                source,
                install_location: location.to_string_lossy().into_owned(),
                auto_update: false,
                last_updated: String::new(),
            }],
            None,
        )
        .unwrap();
        request(&cfg, "marketplace/remove", json!({"name": name}))
            .await
            .unwrap();
        assert_eq!(location.exists(), !should_remove, "{}", location.display());
        assert!(outside.join("keep").exists());
        assert!(local.join("keep").exists());
        assert!(cache.exists());
    }
}
