use peri_acp_types::plugin::McpServerConfig;
use std::{collections::HashMap, time::Duration};

use super::*;

#[tokio::test]
async fn test_successful_write_marks_cache_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());
    let ticket = cache
        .ticket("origin-a", "skills/list", "null")
        .await
        .unwrap();

    cache
        .put_ticket(
            &ticket,
            Duration::from_secs(60),
            &serde_json::json!({"skills": []}),
        )
        .await;

    assert_eq!(
        cache.recent_status("origin-a"),
        Some(CacheLoadStatus::StoredAfterFetch)
    );
}

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

#[test]
fn test_cache_instances_for_same_root_share_recent_status() {
    let dir = tempfile::tempdir().unwrap();
    let first = McpResourceCache::at(dir.path().to_path_buf());
    let second = McpResourceCache::at(dir.path().to_path_buf());

    first.mark_live_fetch("origin-a", "resources/list");
    second.mark_hit("origin-a", "skills/legacy-read", false);

    assert_eq!(
        first.recent_status("origin-a"),
        Some(CacheLoadStatus::McppHit)
    );
    assert_eq!(
        second.recent_status("origin-a"),
        Some(CacheLoadStatus::McppHit)
    );
}

#[tokio::test]
async fn test_cache_instances_for_same_root_share_active_version() {
    let dir = tempfile::tempdir().unwrap();
    let first = McpResourceCache::at(dir.path().to_path_buf());
    let second = McpResourceCache::at(dir.path().to_path_buf());
    let ticket = first
        .ticket("origin-a", "resources/list", "null")
        .await
        .unwrap();
    first
        .put_ticket_versioned(
            &ticket,
            Duration::from_millis(1),
            Some("opaque-v1"),
            &serde_json::json!({"resources": []}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(5)).await;

    first.set_cache_version("origin-a", Some("opaque-v1"));
    let value: Option<serde_json::Value> = second.get("origin-a", "resources/list", "null").await;

    assert_eq!(value, Some(serde_json::json!({"resources": []})));
}

#[tokio::test]
async fn test_skill_cache_status_takes_priority_over_resource_status() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());

    cache.mark_live_fetch("origin-a", "resources/list");
    cache
        .put(
            "origin-a",
            "skills/list",
            "null",
            Duration::from_secs(60),
            &serde_json::json!({"skills": []}),
        )
        .await;
    let _: Option<serde_json::Value> = cache.get("origin-a", "skills/list", "null").await;

    assert_eq!(
        cache.recent_status("origin-a"),
        Some(CacheLoadStatus::McppHit),
        "启动时 resources/list 的实时请求不得掩盖后续 Skill cache hit"
    );
}

#[tokio::test]
async fn test_entry_is_readable_by_new_cache_instance() {
    let dir = tempfile::tempdir().unwrap();
    let first = McpResourceCache::at(dir.path().to_path_buf());
    first
        .put(
            "origin-a",
            "skills/list",
            "null",
            Duration::from_secs(60),
            &serde_json::json!({"skills": []}),
        )
        .await;

    let second = McpResourceCache::at(dir.path().to_path_buf());
    let value: Option<serde_json::Value> = second.get("origin-a", "skills/list", "null").await;
    assert_eq!(value, Some(serde_json::json!({"skills": []})));
}

#[tokio::test]
async fn test_matching_cache_version_reuses_expired_entry() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());
    let ticket = cache
        .ticket("origin-a", "resources/list", "null")
        .await
        .unwrap();
    cache
        .put_ticket_versioned(
            &ticket,
            Duration::from_millis(1),
            Some("opaque-v1"),
            &serde_json::json!({"resources": []}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(5)).await;

    let hit: Option<serde_json::Value> = cache
        .get_versioned("origin-a", "resources/list", "null", Some("opaque-v1"))
        .await;
    assert!(hit.is_some());
}

#[tokio::test]
async fn test_missing_or_mismatched_cache_version_falls_back_to_ttl_miss() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());
    let ticket = cache
        .ticket("origin-a", "resources/list", "null")
        .await
        .unwrap();
    cache
        .put_ticket_versioned(
            &ticket,
            Duration::from_millis(1),
            Some("opaque-v1"),
            &serde_json::json!({"resources": []}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(5)).await;

    let miss: Option<serde_json::Value> = cache
        .get_versioned("origin-a", "resources/list", "null", Some("opaque-v2"))
        .await;
    assert!(miss.is_none());
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

/// 统计 content 中归属本命名空间的条目总字节数与条数。仅验证 `trim_locked`
/// 的“列举—按大小求和—驱逐”数学，不依赖反序列化，读取只查 cacache 列表。
async fn content_total(cache: &McpResourceCache) -> (u64, usize) {
    let mut total = 0u64;
    let mut count = 0;
    for entry in cacache::list_sync(&cache.content_path).filter_map(Result::ok) {
        if entry.key.starts_with(CACHE_NAMESPACE) {
            total += entry.size as u64;
            count += 1;
        }
    }
    (total, count)
}

#[tokio::test]
async fn test_trim_locked_evicts_until_within_budget() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());

    // 写入约 66 MiB 合成 blob（绕过序列化/epoch，精确控字节、轻量），
    // 越过 64 MiB 总量预算，迫使裁剪驱逐最旧条目。
    for i in 0..66u32 {
        let key = format!("{CACHE_NAMESPACE}:syn-{i}");
        cacache::write(&cache.content_path, key, vec![0u8; 1 << 20])
            .await
            .unwrap();
    }

    let (before_total, before_count) = content_total(&cache).await;
    assert!(before_total > MAX_CACHE_BYTES, "前置：写入超过预算");

    cache.trim_locked(0).await;

    let (after_total, after_count) = content_total(&cache).await;
    assert!(after_total <= MAX_CACHE_BYTES, "裁剪后须回到预算内");
    assert!(after_count < before_count, "超预算时必须发生驱逐");
}

#[tokio::test]
async fn test_trim_locked_no_eviction_at_exact_budget() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());

    for i in 0..60u32 {
        let key = format!("{CACHE_NAMESPACE}:syn-{i}");
        cacache::write(&cache.content_path, key, vec![0u8; 1 << 20])
            .await
            .unwrap();
    }

    let (current, count) = content_total(&cache).await;
    // incoming 令 total + incoming == MAX（严格 `>`，恰好达标不驱逐）。
    let incoming = MAX_CACHE_BYTES.saturating_sub(current);

    cache.trim_locked(incoming).await;

    let (after_total, after_count) = content_total(&cache).await;
    assert_eq!(after_count, count, "恰好达标时不应驱逐");
    assert_eq!(
        after_total + incoming,
        MAX_CACHE_BYTES,
        "恰好达标时总量等于预算"
    );
}

#[tokio::test]
async fn test_trim_locked_evicts_when_one_byte_over() {
    let dir = tempfile::tempdir().unwrap();
    let cache = McpResourceCache::at(dir.path().to_path_buf());

    for i in 0..60u32 {
        let key = format!("{CACHE_NAMESPACE}:syn-{i}");
        cacache::write(&cache.content_path, key, vec![0u8; 1 << 20])
            .await
            .unwrap();
    }

    let (current, count) = content_total(&cache).await;
    // 越界一字节：total + incoming == MAX + 1，触发驱逐。
    let incoming = MAX_CACHE_BYTES - current + 1;

    cache.trim_locked(incoming).await;

    let (after_total, after_count) = content_total(&cache).await;
    assert!(after_count < count, "越界一字节必须至少驱逐一条");
    assert!(
        after_total + incoming <= MAX_CACHE_BYTES,
        "裁剪后须回到预算内"
    );
}
