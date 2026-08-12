//! Tests for mid_lsp

use std::collections::HashMap;

use peri_resources::lsp::config::LspServerConfig;

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

/// from_pool 必须复用注入的 pool 实例（H1：会话级共享，不重建）
#[test]
fn test_from_pool_reuses_pool_instance() {
    let config = LspConfigFile {
        lsp_servers: HashMap::new(),
    };
    let pool = std::sync::Arc::new(LspServerPool::new("/tmp", config));
    let mw = LspMiddleware::from_pool(std::sync::Arc::clone(&pool));
    assert!(
        std::sync::Arc::ptr_eq(&pool, &mw.shared_pool()),
        "from_pool 应复用注入的 pool 而非重建"
    );
}

#[test]
fn test_root_uri_normalized_no_double_prefix() {
    // pool.root_uri() 已是完整 file:// URI（绝对化 + percent-encode），
    // 直接使用即可，不应出现 file://file:// 双重前缀。
    // Windows 上 /tmp 落到当前盘根（file:///D:/tmp/...），断言公共前缀与编码
    let config = LspConfigFile {
        lsp_servers: HashMap::new(),
    };
    let mw = LspMiddleware::new("/tmp/my dir".to_string(), config);
    let root = mw.shared_pool().root_uri().to_string();
    assert!(root.starts_with("file:///"), "got {root}");
    assert!(root.contains("/tmp/my%20dir"), "got {root}");
    assert!(!root.starts_with("file://file://"), "双重前缀残留: {root}");
}

#[test]
fn test_root_uri_relative_absolutized() {
    // 相对路径 root_uri 同样绝对化，且不以 file://file:// 开头
    let config = LspConfigFile {
        lsp_servers: HashMap::new(),
    };
    let mw = LspMiddleware::new("relative/path".to_string(), config);
    let root = mw.shared_pool().root_uri().to_string();
    assert!(root.starts_with("file://"), "got {root}");
    assert!(root.ends_with("/relative/path"), "got {root}");
    assert!(!root.starts_with("file://file://"), "双重前缀残留: {root}");
}

#[tokio::test]
async fn test_diagnostics_without_matching_server_returns_error() {
    let mw = LspMiddleware::from_configs(
        "/tmp".to_string(),
        vec![make_config(
            "typescript-language-server",
            vec![(".ts", "typescript")],
        )],
    );
    let tool = <LspMiddleware as Middleware>::collect_tools(&mw, "/tmp")
        .into_iter()
        .next()
        .unwrap();

    let error = tool
        .invoke(
            serde_json::json!({
                "operation": "diagnostics",
                "file_path": "/tmp/main.rs"
            }),
            peri_agent::tools::ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "无 LSP 服务器可处理文件: /tmp/main.rs (扩展名: rs)"
    );
}
