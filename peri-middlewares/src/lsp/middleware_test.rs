//! Tests for mid_lsp

use std::collections::HashMap;

use peri_lsp::config::LspServerConfig;

use super::*;

fn make_config(name: &str, exts: Vec<(&str, &str)>) -> LspServerConfig {
    LspServerConfig {
        name: name.to_string(),
        command: name.to_string(),
        args: vec!["--stdio".to_string()],
        env: None,
        extension_to_language: exts
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        initialization_options: None,
        disabled: None,
        max_restarts: None,
        startup_timeout: None,
        source: None,
    }
}

#[test]
fn test_middleware_name() {
    let config = LspConfigFile {
        lsp_servers: HashMap::new(),
    };
    let mw = LspMiddleware::new("/tmp".to_string(), config);
    assert_eq!(<LspMiddleware as Middleware>::name(&mw), "LspMiddleware");
}

#[test]
fn test_collect_tools_empty_config() {
    let config = LspConfigFile {
        lsp_servers: HashMap::new(),
    };
    let mw = LspMiddleware::new("/tmp".to_string(), config);
    let tools = <LspMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    assert!(tools.is_empty());
}

#[test]
fn test_collect_tools_with_servers() {
    let mut servers = HashMap::new();
    servers.insert(
        "rust-analyzer".to_string(),
        make_config("rust-analyzer", vec![(".rs", "rust")]),
    );
    let config = LspConfigFile {
        lsp_servers: servers,
    };
    let mw = LspMiddleware::new("/tmp".to_string(), config);
    let tools = <LspMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "LSP");
}

#[test]
fn test_shared_pool() {
    let mut servers = HashMap::new();
    servers.insert(
        "rust-analyzer".to_string(),
        make_config("rust-analyzer", vec![(".rs", "rust")]),
    );
    let config = LspConfigFile {
        lsp_servers: servers,
    };
    let mw = LspMiddleware::new("/tmp".to_string(), config);
    let pool = mw.shared_pool();
    assert!(pool.has_servers());
}

#[test]
fn test_from_configs() {
    let configs = vec![make_config("rust-analyzer", vec![(".rs", "rust")])];
    let mw = LspMiddleware::from_configs("/tmp".to_string(), configs);
    assert_eq!(<LspMiddleware as Middleware>::name(&mw), "LspMiddleware");
    assert!(mw.pool.has_servers());
}
