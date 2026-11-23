use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::kit::image_preview::{InlineImage, InlineImageEncoder};
use crate::kit::terminal_caps::{GraphicsProtocol, TerminalCaps};

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

/// 测试夹具能力位：Kitty 不依赖单元格像素尺寸，解码/编码路径可完全离线复现。
fn kitty_caps() -> TerminalCaps {
    TerminalCaps {
        graphics: GraphicsProtocol::Kitty,
        ..TerminalCaps::default()
    }
}

fn sixel_caps() -> TerminalCaps {
    TerminalCaps {
        graphics: GraphicsProtocol::Sixel,
        ..TerminalCaps::default()
    }
}

fn encoder(caps: &TerminalCaps, cell_px: Option<(u16, u16)>) -> InlineImageEncoder {
    InlineImageEncoder::for_caps(caps, cell_px).expect("测试能力位应支持协议")
}

/// 编码一张纯色小图，供缓存状态机测试使用。
fn ready_image() -> Arc<InlineImage> {
    let encoder = encoder(&kitty_caps(), None);
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        8,
        8,
        image::Rgba([220, 30, 30, 255]),
    ));
    Arc::new(
        encoder
            .encode(img, 8, 4)
            .expect("小图应能在单元格预算内编码"),
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
            cache.complete(cache_key, id, Ok(ready_image())),
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
        cache.complete(&key, id + 1, Ok(ready_image())),
        CompleteOutcome::Ignored
    );
    assert!(matches!(
        cache.lookup(&src),
        Some(InlineImageState::Pending)
    ));
    assert_eq!(cache.revision, 0);

    assert_eq!(
        cache.complete(&key, id, Ok(ready_image())),
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
    assert_eq!(cache.bytes, 0, "失败条目不占协议缓存字节");
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
            cache.complete(&key, id, Ok(ready_image())),
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
fn encoded_size_respects_budget_and_never_upscales_beyond_cell() {
    let encoder = encoder(&kitty_caps(), None);
    let rgba = |width: u32, height: u32| {
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([220, 30, 30, 255]),
        ))
    };
    let small = encoder.encode(rgba(4, 4), 112, MAX_ROWS).expect("small");
    assert_eq!((small.cols(), small.rows()), (1, 1), "4x4 像素只占 1 格");

    let tall = encoder
        .encode(rgba(4, 640), 112, MAX_ROWS)
        .expect("tall image");
    assert_eq!(tall.rows(), MAX_ROWS, "高度受 8 行上限约束");
    assert!(tall.cols() >= 1);

    let wide = encoder
        .encode(rgba(4000, 200), 112, MAX_ROWS)
        .expect("wide image");
    assert!(wide.cols() <= 112 && wide.rows() <= MAX_ROWS);

    assert!(encoder.encode(rgba(4, 4), 0, MAX_ROWS).is_none());
    assert!(encoder.encode(rgba(4, 4), 112, 0).is_none());
}

/// Kitty：首帧传输串 + unicode placeholder 进入 buffer，而不是半块字符。
#[test]
fn kitty_refresh_writes_escape_placeholders_not_halfblocks() {
    let encoder = encoder(&kitty_caps(), None);
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        40,
        40,
        image::Rgba([220, 30, 30, 255]),
    ));
    let image = encoder.encode(img, 20, MAX_ROWS).expect("encode");
    let area = Rect::new(2, 1, image.cols(), image.rows());
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 12));
    image.render(area, &mut buf);

    let first = buf[(area.x, area.y)].symbol();
    assert!(first.contains("_G"), "首帧应包含 Kitty 传输序列：{first:?}");
    assert!(
        !first.contains('\u{2580}') && !first.contains('\u{2584}'),
        "像素协议路径不得退化为半块字符"
    );
    // 其余占位行带 unicode placeholder（U+10EEEE），不是空格。
    let second = buf[(area.x, area.y + 1)].symbol();
    assert!(
        second.contains('\u{10EEEE}'),
        "占位行应写入 Kitty unicode placeholder：{second:?}"
    );
}

/// Sixel：DCS 串落在首格，其余单元格标记跳过（不写半块）。
#[test]
fn sixel_refresh_writes_dcs_not_halfblocks() {
    let cell_px = Some((10u16, 20u16));
    let encoder = encoder(&sixel_caps(), cell_px);
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        40,
        40,
        image::Rgba([220, 30, 30, 255]),
    ));
    let image = encoder.encode(img, 20, MAX_ROWS).expect("sixel encode");
    let area = Rect::new(0, 0, image.cols(), image.rows());
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 12));
    image.render(area, &mut buf);

    let first = buf[(0, 0)].symbol();
    assert!(
        first.contains("\u{1b}P") && first.contains('q'),
        "Sixel 首格应为 DCS 串：{first:?}"
    );
    assert!(!first.contains('\u{2580}') && !first.contains('\u{2584}'));
}

/// 不支持协议（None/ITerm2/无单元格尺寸的 Sixel）不请求、不构造 Picker。
#[test]
fn unsupported_caps_never_request_or_ready() {
    for caps in [
        TerminalCaps::default(),
        TerminalCaps {
            graphics: GraphicsProtocol::ITerm2,
            ..TerminalCaps::default()
        },
        sixel_caps(),
    ] {
        assert!(InlineImageEncoder::for_caps(&caps, None).is_none());
        request_with("/tmp/never-requested.png", 20, MAX_ROWS, &caps, None);
    }
    assert!(!cache().lock().inflight.contains_key(&CacheKey {
        path: PathBuf::from("/tmp/never-requested.png"),
        mtime: 0,
        len: 0,
        cols: 20,
        rows: MAX_ROWS,
    }));
    assert!(lookup_ready("/tmp/never-requested.png", 20, MAX_ROWS).is_none());
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
fn production_request_encodes_real_png_for_kitty() {
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
        "后台解码+编码应在超时前完成"
    );
    let ready = lookup_ready(source, target.0, target.1).expect("ready image");
    assert!(ready.cols >= 1 && ready.rows >= 1);
    assert!(ready.rows <= MAX_ROWS);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    ready
        .image
        .render(Rect::new(0, 0, ready.cols, ready.rows), &mut buf);
    assert!(
        buf[(0, 0)].symbol().contains("_G"),
        "窗口内应写入 Kitty 传输序列，实际 {:?}",
        buf[(0, 0)].symbol()
    );
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
        cache.complete(&old, old_id, Ok(ready_image())),
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

    assert!(!cache.aliases.contains_key(&extra_src));

    assert_eq!(
        cache.complete(&key("0.png", 1, 10, 10, 2), ids[0], Ok(ready_image())),
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
    cache.aliases.retain(|_, alias| alias.key != old_key);
    let outcome = cache.complete(&old_key, ids[0], Ok(ready_image()));
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

#[test]
fn failed_images_release_old_keys_and_aliases_without_charging_protocol_bytes() {
    let mut cache = Cache::default();
    for index in 0..=MAX_CACHE_ENTRIES {
        let name = format!("{index}.png");
        let k = key(&name, 0, 0, 10, 2);
        let id = spawned(cache.begin(source(&name, 10, 2), k.clone()));
        cache.complete(&k, id, Err("missing"));
    }

    assert_eq!(cache.bytes, 0);
    assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
    assert_eq!(cache.aliases.len(), MAX_CACHE_ENTRIES);
    assert!(cache.lookup(&source("0.png", 10, 2)).is_none());
    let newest = format!("{MAX_CACHE_ENTRIES}.png");
    assert!(matches!(
        cache.lookup(&source(&newest, 10, 2)),
        Some(InlineImageState::Failed(_))
    ));
}

#[test]
fn many_source_aliases_share_one_entry_with_bounded_metadata() {
    let mut cache = Cache::default();
    let k = key("canonical.png", 1, 10, 10, 2);
    let id = spawned(cache.begin(source("0.png", 10, 2), k.clone()));
    cache.complete(&k, id, Ok(ready_image()));
    for index in 1..=MAX_SOURCE_ALIASES {
        assert_eq!(
            cache.begin(source(&format!("{index}.png"), 10, 2), k.clone()),
            BeginOutcome::Existing
        );
    }

    assert_eq!(cache.entries.len(), 1);
    assert_eq!(cache.aliases.len(), MAX_SOURCE_ALIASES);
    assert!(cache.lookup(&source("0.png", 10, 2)).is_none());
    let newest = format!("{MAX_SOURCE_ALIASES}.png");
    assert!(matches!(
        cache.lookup(&source(&newest, 10, 2)),
        Some(InlineImageState::Ready(_))
    ));
}
