use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ratatui::text::Line;

use super::*;

fn source(name: &str, cols: u16, rows: u16) -> SourceKey {
    SourceKey {
        source: name.to_string(),
        cols,
        rows,
    }
}

fn key(name: &str, mtime: u128, len: u64, cols: u16, rows: u16) -> CacheKey {
    CacheKey {
        path: PathBuf::from(name),
        mtime,
        len,
        cols,
        rows,
    }
}

fn lines(width: usize, rows: usize) -> Arc<Vec<Line<'static>>> {
    Arc::new(
        (0..rows)
            .map(|_| Line::from("█".repeat(width)))
            .collect::<Vec<_>>(),
    )
}

fn spawned(outcome: BeginOutcome) -> u64 {
    match outcome {
        BeginOutcome::Spawn(id) => id,
        other => panic!("expected Spawn, got {other:?}"),
    }
}

#[test]
fn cache_key_distinguishes_mtime_length_and_target() {
    let mut cache = Cache::default();
    let src = source("a.png", 10, 2);
    let keys = [
        key("a.png", 1, 10, 10, 2),
        key("a.png", 2, 10, 10, 2),
        key("a.png", 2, 11, 10, 2),
        key("a.png", 2, 11, 20, 2),
        key("a.png", 2, 11, 20, 3),
    ];
    for (index, cache_key) in keys.iter().enumerate() {
        let id = spawned(cache.begin(src.clone(), cache_key.clone()));
        assert_eq!(
            cache.complete(cache_key, id, Ok(lines(2, 1))),
            CompleteOutcome::Updated,
            "第 {index} 个键应独立完成（mtime/len/目标尺寸任一变化都是新版本）"
        );
        assert!(matches!(
            cache.begin(src.clone(), cache_key.clone()),
            BeginOutcome::Existing
        ));
    }
    assert_eq!(cache.entries.len(), 5);
}

#[test]
fn stale_completion_is_ignored_and_does_not_bump_revision() {
    let mut cache = Cache::default();
    let src = source("a.png", 10, 2);
    let key = key("a.png", 1, 10, 10, 2);
    let id = spawned(cache.begin(src.clone(), key.clone()));
    assert_eq!(
        cache.complete(&key, id + 1, Ok(lines(2, 1))),
        CompleteOutcome::Ignored
    );
    assert!(matches!(
        cache.lookup(&src),
        Some(InlineImageState::Pending)
    ));
    assert_eq!(cache.revision, 0);

    assert_eq!(
        cache.complete(&key, id, Ok(lines(2, 1))),
        CompleteOutcome::Updated
    );
    assert!(matches!(
        cache.lookup(&src),
        Some(InlineImageState::Ready(_))
    ));
    assert_eq!(cache.revision, 1);
}

#[test]
fn failed_result_is_cached_and_falls_back() {
    let mut cache = Cache::default();
    let src = source("bad.png", 10, 2);
    let key = key("bad.png", 1, 10, 10, 2);
    let id = spawned(cache.begin(src.clone(), key.clone()));
    assert_eq!(
        cache.complete(&key, id, Err("format")),
        CompleteOutcome::Updated
    );
    match cache.lookup(&src) {
        Some(InlineImageState::Failed(reason)) => assert_eq!(reason, "format"),
        other => panic!("expected failed state, got {other:?}"),
    }
    assert_eq!(cache.bytes, 0, "失败条目不占行缓存字节");
}

#[test]
fn duplicate_requests_share_one_pending_entry() {
    let mut cache = Cache::default();
    let src = source("a.png", 10, 2);
    let key = key("a.png", 1, 10, 10, 2);
    let first = cache.begin(src.clone(), key.clone());
    let second = cache.begin(src, key.clone());
    assert!(matches!(first, BeginOutcome::Spawn(_)));
    assert!(
        matches!(second, BeginOutcome::Existing),
        "同版本同尺寸重复请求必须去重"
    );
    assert_eq!(cache.entries.len(), 1);
    assert_eq!(cache.inflight.len(), 1);
}

#[test]
fn lru_evicts_least_recently_used() {
    let mut cache = Cache::default();
    let entries = [("a.png", 1u128, 1u64), ("b.png", 2, 2), ("c.png", 3, 3)];
    for (name, mtime, len) in entries {
        let src = source(name, 10, 2);
        let key = key(name, mtime, len, 10, 2);
        let id = spawned(cache.begin(src, key.clone()));
        assert_eq!(
            cache.complete(&key, id, Ok(lines(4, 2))),
            CompleteOutcome::Updated
        );
    }
    // 触碰 a（最近使用），b 成为最旧。
    let a = source("a.png", 10, 2);
    assert!(cache.lookup(&a).is_some());
    let before = cache.bytes;
    assert!(before > 0);

    cache.evict_to_budget(before - 1);
    assert!(cache.lookup(&a).is_some(), "最近使用的 a 保留");
    assert!(
        cache.lookup(&source("b.png", 10, 2)).is_none(),
        "最旧的 b 被淘汰"
    );
    assert!(cache.lookup(&source("c.png", 10, 2)).is_some());
    assert!(cache.bytes < before);
}

#[test]
fn target_size_caps_rows_and_never_upscales_small_images() {
    assert_eq!(fit_target(4, 4, 112, 8), Some((4, 2)));
    assert_eq!(fit_target(4, 4, 2, 8), Some((2, 1)));
    let tall = fit_target(4, 64, 112, MAX_ROWS);
    assert_eq!(tall, Some((1, MAX_ROWS)), "高度受 8 行上限约束");
    let wide = fit_target(400, 200, 112, MAX_ROWS).expect("wide fit");
    assert!(wide.0 <= 112 && wide.1 <= MAX_ROWS);
    assert_eq!(fit_target(0, 4, 112, 8), None);
    assert_eq!(fit_target(4, 4, 0, 8), None);
    assert_eq!(fit_target(4, 4, 112, 0), None);
}

#[test]
fn standalone_image_line_requires_exclusive_syntax() {
    assert_eq!(
        standalone_image_url("![alt](/tmp/a.png)"),
        Some("/tmp/a.png".to_string())
    );
    assert_eq!(
        standalone_image_url("  ![alt](a%20b.png)  "),
        Some("a%20b.png".to_string())
    );
    assert_eq!(standalone_image_url("see ![alt](a.png)"), None);
    assert_eq!(standalone_image_url("![a](p) ![b](q)"), None);
    assert_eq!(standalone_image_url("plain text"), None);
}

#[test]
fn percent_decodes_local_file_uri() {
    assert_eq!(percent_decode("a%20b.png"), "a b.png");
    assert_eq!(percent_decode("a%2Fb.png"), "a/b.png");
    assert_eq!(percent_decode("100%.png"), "100%.png");
}

#[test]
fn production_request_decodes_real_png_into_halfblocks() {
    let dir = tempfile::tempdir().expect("inline image production request: tempdir must exist");
    let file = dir.path().join("red.png");
    let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([220, 30, 30, 255]));
    image
        .save_with_format(&file, image::ImageFormat::Png)
        .expect("inline image production request: png fixture must encode");
    let source = file.to_str().expect("utf8 path");
    let target = (20u16, MAX_ROWS);
    assert!(
        request_until_ready(source, target.0, target.1, Duration::from_secs(5)),
        "后台解码应在超时前完成"
    );
    let rendered = lookup_ready(source, target.0, target.1).expect("ready lines");
    assert!(!rendered.is_empty());
    assert!(
        rendered.iter().any(|line| line.spans.iter().any(|span| {
            span.style.fg.is_some() && span.style.bg.is_some() && span.content.contains('█')
        })),
        "半块行应带前景/背景色"
    );
    assert!(rendered.len() <= usize::from(MAX_ROWS));
}

#[test]
fn remote_and_dangerous_sources_are_never_requested() {
    request("https://example.com/a.png", 20, MAX_ROWS);
    request("javascript:alert(1)", 20, MAX_ROWS);
    assert!(lookup_ready("https://example.com/a.png", 20, MAX_ROWS).is_none());
    assert!(lookup_ready("javascript:alert(1)", 20, MAX_ROWS).is_none());
}

#[test]
fn abandoned_pending_entry_is_released_for_retry() {
    let mut cache = Cache::default();
    let src = source("link.png", 10, 2);
    let old = key("link.png", 1, 10, 10, 2);
    let new = key("link.png", 2, 10, 10, 2);
    let old_id = spawned(cache.begin(src.clone(), old.clone()));
    // 同一 source 指向新版本（如符号链接切换）：旧别名被取代。
    let _new_id = spawned(cache.begin(src.clone(), new));
    assert_eq!(
        cache.complete(&old, old_id, Ok(lines(2, 1))),
        CompleteOutcome::Released
    );
    assert!(
        CompleteOutcome::Released.wakes_render(),
        "丢弃结果释放了槽位，必须唤醒 effect 重试暂缓请求"
    );
    assert!(
        !cache.entries.contains_key(&old),
        "被取代的 Pending 条目必须清理"
    );
    assert!(
        matches!(cache.begin(src, old), BeginOutcome::Spawn(_)),
        "同版本再次出现时应能重新启动 worker"
    );
}

#[test]
fn concurrent_decodes_are_bounded_and_retried_later() {
    let mut cache = Cache::default();
    let mut ids = Vec::new();
    for index in 0..MAX_CONCURRENT {
        let name = format!("{index}.png");
        let src = source(&name, 10, 2);
        ids.push(spawned(cache.begin(src, key(&name, 1, 10, 10, 2))));
    }
    let extra_src = source("extra.png", 10, 2);
    let extra_key = key("extra.png", 1, 10, 10, 2);
    assert!(matches!(
        cache.begin(extra_src.clone(), extra_key.clone()),
        BeginOutcome::Deferred
    ));
    assert!(
        !cache.entries.contains_key(&extra_key),
        "暂缓请求不登记条目，等下一次 effect 重试"
    );

    assert_eq!(
        cache.complete(&key("0.png", 1, 10, 10, 2), ids[0], Ok(lines(2, 1))),
        CompleteOutcome::Updated
    );
    assert!(
        matches!(cache.begin(extra_src, extra_key), BeginOutcome::Spawn(_)),
        "腾出槽位后重试应启动"
    );
}

#[test]
fn discarded_completion_also_frees_slot_and_wakes_render() {
    let mut cache = Cache::default();
    let mut ids = Vec::new();
    for index in 0..MAX_CONCURRENT {
        let name = format!("{index}.png");
        let src = source(&name, 10, 2);
        ids.push(spawned(cache.begin(src, key(&name, 1, 10, 10, 2))));
    }
    let extra_src = source("extra.png", 10, 2);
    let extra_key = key("extra.png", 1, 10, 10, 2);
    assert!(matches!(
        cache.begin(extra_src.clone(), extra_key.clone()),
        BeginOutcome::Deferred
    ));

    // 来源切到新版本后旧键不再被引用：完成结果被丢弃，但槽位确实释放了。
    let old_key = key("0.png", 1, 10, 10, 2);
    cache.aliases.retain(|_, aliased| aliased != &old_key);
    let outcome = cache.complete(&old_key, ids[0], Ok(lines(2, 1)));
    assert_eq!(outcome, CompleteOutcome::Released);
    assert!(
        outcome.wakes_render(),
        "丢弃结果也必须唤醒 effect，否则暂缓请求要等滚动才重试"
    );
    assert!(
        matches!(cache.begin(extra_src, extra_key), BeginOutcome::Spawn(_)),
        "丢弃结果腾出的槽位应允许暂缓请求重试"
    );
}
