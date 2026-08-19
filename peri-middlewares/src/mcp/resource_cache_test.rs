use std::time::Duration;

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
async fn test_resource_update_invalidates_only_matching_uri() {
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
    assert_eq!(second, Some(serde_json::json!("resource://two")));
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
    let origin = cache_origin("server", Some("https://example.test/path?token=secret"));
    assert!(origin.starts_with("mcp-origin:"));
    assert!(!origin.contains("example.test"));
    assert!(!origin.contains("secret"));
}
