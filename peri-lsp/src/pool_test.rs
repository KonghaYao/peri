//! Tests for pool

use std::collections::HashMap;

use super::*;
use crate::config::{LspConfigFile, LspServerConfig};

fn make_config() -> LspConfigFile {
    let mut servers = HashMap::new();
    servers.insert(
        "rust-analyzer".to_string(),
        LspServerConfig {
            name: "rust-analyzer".to_string(),
            command: "rust-analyzer".to_string(),
            args: vec!["--stdio".to_string()],
            env: None,
            extension_to_language: HashMap::from([(".rs".to_string(), "rust".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    servers.insert(
        "typescript".to_string(),
        LspServerConfig {
            name: "typescript-language-server".to_string(),
            command: "typescript-language-server".to_string(),
            args: vec!["--stdio".to_string()],
            env: None,
            extension_to_language: HashMap::from([
                (".ts".to_string(), "typescript".to_string()),
                (".tsx".to_string(), "typescriptreact".to_string()),
            ]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: None,
            source: None,
        },
    );
    LspConfigFile {
        lsp_servers: servers,
    }
}

#[test]
fn test_extension_routing() {
    let pool = LspServerPool::new("/tmp", make_config());
    assert!(pool.server_for_file("/test/main.rs").is_some());
    assert!(pool.server_for_file("/test/index.ts").is_some());
    assert!(pool.server_for_file("/test/App.tsx").is_some());
    assert!(pool.server_for_file("/test/readme.md").is_none());
    assert!(pool.server_for_file("/test/no_ext").is_none());
}

#[test]
fn test_case_insensitive_extension() {
    let pool = LspServerPool::new("/tmp", make_config());
    assert!(pool.server_for_file("/test/main.RS").is_some());
    assert!(pool.server_for_file("/test/main.TS").is_some());
}

#[test]
fn test_disabled_server() {
    let mut config = make_config();
    config
        .lsp_servers
        .get_mut("rust-analyzer")
        .unwrap()
        .disabled = Some(true);
    let pool = LspServerPool::new("/tmp", config);
    assert!(pool.server_for_file("/test/main.rs").is_none());
}

#[test]
fn test_has_servers() {
    let pool = LspServerPool::new("/tmp", make_config());
    assert!(pool.has_servers());
}

#[test]
fn test_empty_config() {
    let pool = LspServerPool::new("/tmp", LspConfigFile::default());
    assert!(!pool.has_servers());
    assert!(pool.server_for_file("/test/main.rs").is_none());
}

#[tokio::test]
async fn test_ensure_server_for_file_no_match() {
    let pool = LspServerPool::new("/tmp", make_config());
    // .md 文件没有匹配的 LSP 服务器
    let result = pool.ensure_server_for_file("/test/readme.md").await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("readme.md"));
}

#[tokio::test]
async fn test_ensure_server_for_file_already_initialized() {
    let pool = LspServerPool::new("/tmp", make_config());
    // 手动标记为已初始化
    pool.initialized.write().insert("rust-analyzer".to_string());
    // 不应尝试启动
    let result = pool.ensure_server_for_file("/test/main.rs").await;
    assert!(result.is_ok());
    // typescript 仍然未初始化
    assert!(!pool.initialized.read().contains("typescript"));
}
