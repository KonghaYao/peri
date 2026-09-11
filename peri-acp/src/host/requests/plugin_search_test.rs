//! Real request dispatch against temporary marketplace cache files.
use super::*;

fn search_config(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let config = make_peri_config_with_provider(make_provider_config(
        "fixture",
        "openai",
        "fixture-unused",
        "fixture-model",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, tmp);
    let mut manager = MockPluginManager::install_ok("unused");
    manager.cache_dir = tmp.path().join("marketplaces");
    cfg.plugin_manager = Arc::new(manager);
    cfg
}

fn write_search_catalog(cache: &Path, marketplace: &str, nested: bool) {
    let root = cache.join(marketplace);
    let manifest = if nested {
        root.join(".claude-plugin/marketplace.json")
    } else {
        root.join("marketplace.json")
    };
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(
        manifest,
        serde_json::to_vec(&json!({
            "name": marketplace,
            "plugins": [{
                "name": "compiler-helper", "version": "1.2.3",
                "description": "Inspect build errors", "source": "./plugins/compiler-helper"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
}

async fn request_search(cfg: &AcpServerConfig, query: &str) -> Value {
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    handle_request(
        "plugin/search",
        &json!({"query": query}),
        cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn test_plugin_search_handler_reads_root_and_claude_plugin_layouts() {
    for nested in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = search_config(&tmp);
        write_search_catalog(&cfg.plugin_manager.cache_dir(), "local-catalog", nested);
        let response = request_search(&cfg, "COMPILER").await;
        assert_eq!(
            response["results"],
            json!([{
                "name": "compiler-helper", "version": "1.2.3",
                "description": "Inspect build errors", "marketplace": "local-catalog"
            }]),
            "nested layout = {nested}"
        );
    }
}

#[tokio::test]
async fn test_plugin_search_handler_matches_marketplace_name() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = search_config(&tmp);
    write_search_catalog(&cfg.plugin_manager.cache_dir(), "team-catalog", false);
    let response = request_search(&cfg, "TEAM-CATALOG").await;
    assert_eq!(response["results"].as_array().unwrap().len(), 1);
    assert_eq!(response["results"][0]["name"], "compiler-helper");
    assert_eq!(response["results"][0]["marketplace"], "team-catalog");
}

#[tokio::test]
async fn test_plugin_search_handler_returns_explicit_empty_results_for_no_match() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = search_config(&tmp);
    write_search_catalog(&cfg.plugin_manager.cache_dir(), "team-catalog", false);
    assert_eq!(
        request_search(&cfg, "no-such-entry").await,
        json!({"results": []})
    );
}
