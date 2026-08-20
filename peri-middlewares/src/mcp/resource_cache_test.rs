use peri_acp_types::plugin::McpServerConfig;
use std::{collections::HashMap, time::Duration};

use super::*;

#[tokio::test]
async fn test_public_entry_round_trips_while_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());

    cache
        .put(
            "origin-a",
            "resources/read",
            "resource://one",
            Duration::from_secs(60),
            &serde_json::json!({"contents": ["one"]}),
        )
        .await;

    let value: Option<serde_json::Value> = cache
        .get("origin-a", "resources/read", "resource://one")
        .await;
    assert_eq!(value, Some(serde_json::json!({"contents": ["one"]})));
}

#[tokio::test]
async fn test_expired_entry_is_not_returned() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());

    cache
        .put(
            "origin-a",
            "resources/read",
            "resource://one",
            Duration::from_millis(1),
            &serde_json::json!("value"),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(5)).await;

    let value: Option<serde_json::Value> = cache
        .get("origin-a", "resources/read", "resource://one")
        .await;
    assert!(value.is_none());
}

#[tokio::test]
async fn test_resource_update_stales_cache_globally_for_cross_process_safety() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());
    for uri in ["resource://one", "resource://two"] {
        cache
            .put(
                "origin-a",
                "resources/read",
                uri,
                Duration::from_secs(60),
                &serde_json::json!(uri),
            )
            .await;
    }

    cache
        .invalidate("origin-a", "resources/read", Some("resource://one"))
        .await;

    let first: Option<serde_json::Value> = cache
        .get("origin-a", "resources/read", "resource://one")
        .await;
    let second: Option<serde_json::Value> = cache
        .get("origin-a", "resources/read", "resource://two")
        .await;
    assert!(first.is_none());
    assert!(
        second.is_none(),
        "v2 全局 epoch 保证跨进程失效原子性；更新通知会保守地使同一 cache root 的旧条目全部 stale"
    );
}

#[tokio::test]
async fn test_list_invalidation_stales_all_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());
    for cursor in ["", "cursor-2"] {
        cache
            .put(
                "origin-a",
                "resources/list",
                cursor,
                Duration::from_secs(60),
                &serde_json::json!(cursor),
            )
            .await;
    }

    cache.invalidate("origin-a", "resources/list", None).await;

    let first: Option<serde_json::Value> = cache.get("origin-a", "resources/list", "").await;
    let second: Option<serde_json::Value> =
        cache.get("origin-a", "resources/list", "cursor-2").await;
    assert!(first.is_none());
    assert!(second.is_none());
}

#[test]
fn test_cache_origin_does_not_expose_endpoint() {
    let config = McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: Some("https://example.test/path?token=secret".to_string()),
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
        source: None,
    };
    let origin = cache_origin("server", Some(&config));
    assert!(origin.starts_with("mcp-origin:"));
    assert!(!origin.contains("example.test"));
    assert!(!origin.contains("secret"));
}

#[tokio::test]
async fn test_inflight_response_cannot_revive_invalidated_entry() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());
    let ticket = cache
        .ticket("origin-a", "resources/read", "resource://one")
        .await
        .expect("测试 cache 应可用");

    cache
        .invalidate("origin-a", "resources/read", Some("resource://one"))
        .await;
    cache
        .put_ticket(
            &ticket,
            Duration::from_secs(60),
            &serde_json::json!("stale"),
        )
        .await;

    let value: Option<serde_json::Value> = cache
        .get("origin-a", "resources/read", "resource://one")
        .await;
    assert!(value.is_none(), "失效前取得的响应不得在通知后复活");
}

#[tokio::test]
async fn test_method_invalidation_rejects_inflight_pagination_response() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());
    let ticket = cache
        .ticket("origin-a", "resources/list", "cursor-2")
        .await
        .expect("测试 cache 应可用");

    cache.invalidate("origin-a", "resources/list", None).await;
    cache
        .put_ticket(
            &ticket,
            Duration::from_secs(60),
            &serde_json::json!("stale"),
        )
        .await;

    let value: Option<serde_json::Value> =
        cache.get("origin-a", "resources/list", "cursor-2").await;
    assert!(value.is_none(), "list_changed 后旧分页响应不得写回缓存");
}

#[test]
fn test_stdio_cache_origin_changes_with_config_identity() {
    let first = McpServerConfig {
        command: Some("first-server".to_string()),
        args: Some(vec!["--project-a".to_string()]),
        env: Some(HashMap::from([(String::from("MODE"), String::from("one"))])),
        // 运行时仍优先使用 stdio；该 URL 不能把 origin 错误归类为 HTTP。
        url: Some("https://unused.example.test/mcp?token=secret".to_string()),
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
        source: None,
    };
    let second = McpServerConfig {
        command: Some("second-server".to_string()),
        ..first.clone()
    };
    assert_ne!(
        cache_origin("filesystem", Some(&first)),
        cache_origin("filesystem", Some(&second)),
        "command + url 同存时仍以实际 stdio 配置隔离磁盘缓存"
    );
    let stdio_without_url = McpServerConfig {
        url: None,
        ..first.clone()
    };
    assert_eq!(
        cache_origin("filesystem", Some(&first)),
        cache_origin("filesystem", Some(&stdio_without_url)),
        "未实际使用的 URL 不得改变 stdio server 的缓存身份"
    );
}

#[tokio::test]
async fn test_entry_larger_than_limit_is_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());
    let oversized = "x".repeat(1024 * 1024);

    cache
        .put(
            "origin-a",
            "resources/read",
            "resource://oversized",
            Duration::from_secs(60),
            &oversized,
        )
        .await;

    let value: Option<String> = cache
        .get("origin-a", "resources/read", "resource://oversized")
        .await;
    assert!(value.is_none(), "超过 1 MiB 的单条响应不得持久化");
}

#[test]
fn test_cache_directory_uses_peri_home() {
    let cache = McpResourceCache::new();
    assert!(cache.content_path.ends_with(".peri/cache/mcp/v2/content"));
    assert!(cache.state_path.ends_with(".peri/cache/mcp/v2/state"));
}
