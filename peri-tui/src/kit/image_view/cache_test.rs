//! 统一缓存：键（版本/预算/单元格像素）、LRU、上传仓库、碰撞与生命周期。
//!
//! 编码路径用 Kitty 能力位 + 显式单元格像素，完全离线；像素渲染是否正确留给
//! `image_preview_test`，这里只断言缓存状态机与几何契约。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::kit::image_preview::{InlineImage, InlineImageEncoder};
use crate::kit::terminal_caps::{GraphicsProtocol, TerminalCaps};

use super::*;
use crate::kit::image_view::sizing;
use crate::kit::image_view::{UploadMedia, digest_of};

fn source(name: &str) -> SourceRef {
    SourceRef::File {
        source: name.to_string(),
        cols: 10,
        max_rows: 2,
        cell_px: (10, 20),
    }
}

fn key(
    name: &str,
    mtime: u128,
    len: u64,
    cols: u16,
    max_rows: u16,
    cell_px: (u16, u16),
) -> CacheKey {
    CacheKey {
        source: SourceId::File {
            path: PathBuf::from(name),
            mtime,
            len,
        },
        cols,
        max_rows,
        cell_px,
    }
}

fn kitty_caps() -> TerminalCaps {
    TerminalCaps {
        graphics: GraphicsProtocol::Kitty,
        ..TerminalCaps::default()
    }
}

fn encoder(cell_px: Option<(u16, u16)>) -> InlineImageEncoder {
    InlineImageEncoder::for_caps(&kitty_caps(), cell_px).expect("Kitty 应支持协议")
}

fn ready_image() -> Arc<InlineImage> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        8,
        8,
        image::Rgba([220, 30, 30, 255]),
    ));
    Arc::new(
        encoder(Some((10, 20)))
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

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        width,
        height,
        image::Rgba([30, 120, 200, 255]),
    ));
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .expect("测试图片应可编码为 PNG");
    bytes
}

fn png_file(width: u32, height: u32) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fixture.png");
    std::fs::write(&path, png_bytes(width, height)).expect("write png");
    let source = path.to_string_lossy().into_owned();
    (dir, source)
}

// ── 键与状态机 ────────────────────────────────────────────────────────────

#[test]
fn cache_key_distinguishes_version_budget_and_cell_px() {
    let base = key("a.png", 1, 10, 40, 6, (10, 20));
    assert_eq!(base, key("a.png", 1, 10, 40, 6, (10, 20)));
    assert_ne!(base, key("a.png", 2, 10, 40, 6, (10, 20)), "mtime 变化");
    assert_ne!(base, key("a.png", 1, 11, 40, 6, (10, 20)), "长度变化");
    assert_ne!(base, key("a.png", 1, 10, 39, 6, (10, 20)), "预算列变化");
    assert_ne!(base, key("a.png", 1, 10, 40, 7, (10, 20)), "预算行变化");
    assert_ne!(
        base,
        key("a.png", 1, 10, 40, 6, (8, 17)),
        "单元格像素变化必须换键（字体/缩放后重编码）"
    );
}

#[test]
fn cell_px_change_starts_a_second_entry() {
    let mut cache = Cache::default();
    let src = source("a.png");
    let first = key("a.png", 1, 10, 40, 6, (10, 20));
    let second = key("a.png", 1, 10, 40, 6, (8, 17));
    assert!(matches!(
        cache.begin(src.clone(), first),
        BeginOutcome::Spawn(_)
    ));
    assert!(
        matches!(cache.begin(src, second), BeginOutcome::Spawn(_)),
        "单元格像素变化后同一预算必须重新编码"
    );
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn stale_completion_is_ignored_and_does_not_bump_revision() {
    let mut cache = Cache::default();
    let src = source("a.png");
    let key = key("a.png", 1, 10, 40, 6, (10, 20));
    let id = spawned(cache.begin(src, key.clone()));
    assert_eq!(
        cache.complete(&key, id + 1, Ok(ready_image())),
        CompleteOutcome::Ignored
    );
    assert!(matches!(
        cache.lookup(source("a.png")),
        Some(ImageState::Pending)
    ));
    assert_eq!(cache.revision, 0);
    assert_eq!(
        cache.complete(&key, id, Ok(ready_image())),
        CompleteOutcome::Updated
    );
    assert_eq!(cache.revision, 1);
}

#[test]
fn failed_result_is_cached_and_falls_back() {
    let mut cache = Cache::default();
    let src = source("a.png");
    let key = key("a.png", 1, 10, 40, 6, (10, 20));
    let id = spawned(cache.begin(src.clone(), key.clone()));
    assert_eq!(
        cache.complete(&key, id, Err("format")),
        CompleteOutcome::Updated
    );
    assert!(matches!(
        cache.lookup(src),
        Some(ImageState::Failed("format"))
    ));
    assert_eq!(cache.bytes, 0, "失败条目不占协议缓存字节");
}

/// m10：瞬态失败（线程 spawn 失败 / 解码 panic）不得永久缓存为 Failed——
/// 先回 Pending 并唤醒 effect 重试，到上限后才固定降级。
#[test]
fn transient_failures_retry_then_cache_failed() {
    let mut cache = Cache::default();
    let src = source("a.png");
    let key = key("a.png", 1, 10, 10, 2, (10, 20));
    let id = spawned(cache.begin(src.clone(), key.clone()));
    assert_eq!(
        cache.complete(&key, id, Err("spawn")),
        CompleteOutcome::Retrying
    );
    assert!(
        matches!(cache.lookup(src.clone()), Some(ImageState::Pending)),
        "spawn 失败后条目必须留在 Pending"
    );
    assert_eq!(cache.revision, 1, "重试也要唤醒渲染 effect");

    let id = spawned(cache.begin(src.clone(), key.clone()));
    assert_eq!(
        cache.complete(&key, id, Err("panic")),
        CompleteOutcome::Updated
    );
    assert!(
        matches!(cache.lookup(src.clone()), Some(ImageState::Failed("panic"))),
        "重试上限后固定失败，避免无限起线程"
    );
    assert!(
        matches!(cache.begin(src, key), BeginOutcome::Existing),
        "Failed 后不再派发"
    );
}

/// m9：上传载荷复用 [`MAX_IMAGE_BYTES`] 上限，超限在 base64 解码前快速失败。
#[test]
fn oversized_upload_fails_fast_before_decode() {
    let max_payload_chars = MAX_IMAGE_BYTES.div_ceil(3) as usize * 4;
    let oversized = "A".repeat(max_payload_chars + 1);
    assert_eq!(decode_upload(&oversized), Err("too-large"));
    assert_eq!(decode_upload("!!!!"), Err("base64"), "上限内保持原解码错误");
}

#[test]
fn duplicate_requests_share_one_pending_entry() {
    let mut cache = Cache::default();
    let src = source("a.png");
    let key = key("a.png", 1, 10, 40, 6, (10, 20));
    assert!(matches!(
        cache.begin(src.clone(), key.clone()),
        BeginOutcome::Spawn(_)
    ));
    assert!(matches!(cache.begin(src, key), BeginOutcome::Existing));
    assert_eq!(cache.entries.len(), 1);
    assert_eq!(cache.inflight.len(), 1);
}

#[test]
fn lru_evicts_least_recently_used() {
    let mut cache = Cache::default();
    let entries = [("a.png", 1u128, 1u64), ("b.png", 2, 2), ("c.png", 3, 3)];
    for (name, mtime, len) in entries {
        let src = source(name);
        let key = key(name, mtime, len, 10, 2, (10, 20));
        let id = spawned(cache.begin(src, key.clone()));
        assert_eq!(
            cache.complete(&key, id, Ok(ready_image())),
            CompleteOutcome::Updated
        );
    }
    // 触碰 a（最近使用），b 成为最旧。
    let a = source("a.png");
    assert!(cache.lookup(a.clone()).is_some());
    let before = cache.bytes;
    assert!(before > 0);

    cache.evict_to_budget(before - 1);
    assert!(cache.lookup(a).is_some(), "最近使用的 a 保留");
    assert!(cache.lookup(source("b.png")).is_none(), "最旧的 b 被淘汰");
    assert!(cache.lookup(source("c.png")).is_some());
    assert!(cache.bytes < before);
}

// ── 上传仓库 ──────────────────────────────────────────────────────────────

#[test]
fn uploads_are_bounded_and_released() {
    let mut cache = Cache::default();
    let id = UploadId::from_payload("AAAA", "image/png");
    cache.retain_upload(id, Arc::from("AAAA"));
    assert_eq!(cache.upload(id.digest).as_deref(), Some("AAAA"));
    assert_eq!(cache.upload_bytes, 4);

    assert!(cache.release_all_uploads(), "有载荷时应报告变化");
    assert!(cache.upload(id.digest).is_none());
    assert_eq!(cache.upload_bytes, 0);
    assert!(!cache.release_all_uploads());
}

#[test]
fn upload_digest_collision_drops_old_payload_and_epoch() {
    let mut cache = Cache::default();
    let id = UploadId::from_payload("AAAA", "image/png");
    let epoch = cache.upload_epoch;
    cache.retain_upload(id, Arc::from("AAAA"));
    // 同一摘要（人为碰撞）不同载荷：旧值不可信，epoch 递增使旧编码不可达。
    // 注意这只保证旧编码失效：视图模型只带摘要，旧气泡仍可能显示新载荷。
    cache.retain_upload(id, Arc::from("ZZZZ"));
    assert_eq!(cache.upload(id.digest).as_deref(), Some("ZZZZ"));
    assert_eq!(cache.upload_epoch, epoch + 1, "碰撞必须递增 epoch");
    assert_eq!(cache.upload_bytes, 4, "旧载荷字节已回收");
}

#[test]
fn upload_eviction_keeps_recent_payload() {
    let mut cache = Cache::default();
    let first = UploadId::from_payload("AAAAAAAA", "image/png");
    cache.retain_upload(first, Arc::from("AAAAAAAA"));
    cache.evict_uploads();
    assert!(cache.upload(first.digest).is_some(), "仅一条时不淘汰");
}

// ── 编码契约（真实解码路径）────────────────────────────────────────────────

#[test]
fn scaled_encoding_fills_budget_within_upscale_cap() {
    let encoder = encoder(Some((10, 20)));
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        400,
        200,
        image::Rgba([1, 2, 3, 255]),
    ));
    let cells = sizing::fit(
        img.width(),
        img.height(),
        (10, 20),
        Budget::new(40, 6),
        Fill::Upscale2x,
    )
    .expect("geometry");
    assert_eq!((cells.cols, cells.rows), (24, 6), "受限轴贴边、等比");
    let encoded = encoder
        .encode_scaled(img, cells.cols, cells.rows)
        .expect("encode");
    assert_eq!((encoded.cols(), encoded.rows()), (24, 6));
    assert!(encoded.cols() <= 40 && encoded.rows() <= 6);
}

#[test]
#[serial_test::serial]
fn file_request_encodes_and_fills_geometry() {
    let (_dir, source) = png_file(200, 100);
    let budget = Budget::new(40, 6);
    assert!(request_until_ready(
        ImageSource::File(&source),
        budget,
        Duration::from_secs(10)
    ));
    let placement = lookup(&ImageSource::File(&source), budget).expect("Ready");
    assert_eq!(placement.rows, 6, "高度受限轴贴满 6 行");
    assert!(placement.cols <= 40);
    assert_eq!(
        placement.x_offset,
        (40 - placement.cols) / 2,
        "水平居中偏移"
    );
}

#[test]
#[serial_test::serial]
fn upload_request_encodes_from_repo() {
    let payload = Arc::<str>::from(base64_encode(&png_bytes(64, 32)));
    let id = UploadId {
        digest: digest_of(&payload),
        media: UploadMedia::Png,
    };
    retain_upload(id, Arc::clone(&payload));
    let budget = Budget::new(30, 6);
    assert!(request_until_ready(
        ImageSource::Uploaded(id),
        budget,
        Duration::from_secs(10)
    ));
    let placement = lookup(&ImageSource::Uploaded(id), budget).expect("Ready");
    assert!(placement.cols <= 30 && placement.rows <= 6);
    assert!(placement.cols >= 1 && placement.rows >= 1);
    release_all_uploads();
    assert!(
        lookup(&ImageSource::Uploaded(id), budget).is_none(),
        "会话边界后上传图片不可再查表"
    );
}

#[test]
#[serial_test::serial]
fn unsupported_caps_never_request_or_ready() {
    let (_dir, source) = png_file(16, 16);
    let budget = Budget::new(20, 4);
    for caps in [
        TerminalCaps::default(),
        TerminalCaps {
            graphics: GraphicsProtocol::ITerm2,
            ..TerminalCaps::default()
        },
        TerminalCaps {
            graphics: GraphicsProtocol::Sixel,
            ..TerminalCaps::default()
        },
    ] {
        request(ImageSource::File(&source), budget, &caps, None);
        assert!(cache().lock().inflight.is_empty(), "{caps:?} 不得登记请求");
        assert!(lookup(&ImageSource::File(&source), budget).is_none());
    }
}

#[test]
fn failed_upload_missing_payload_is_noop_and_degrades() {
    let id = UploadId {
        digest: 42,
        media: UploadMedia::Png,
    };
    request(
        ImageSource::Uploaded(id),
        Budget::new(20, 4),
        &kitty_caps(),
        Some((10, 20)),
    );
    assert!(cache().lock().inflight.is_empty(), "缺失载荷不登记请求");
    assert!(lookup(&ImageSource::Uploaded(id), Budget::new(20, 4)).is_none());
}

#[test]
fn failed_state_is_visible_to_layout() {
    let (_dir, source) = png_file(16, 16);
    let budget = Budget::new(20, 4);
    assert!(request_until_ready(
        ImageSource::File(&source),
        budget,
        Duration::from_secs(10)
    ));
    assert!(!failed(&ImageSource::File(&source), budget));
    assert!(!failed(
        &ImageSource::File("/definitely/missing.png"),
        budget
    ));
}

/// m1：上传 alias 键含媒体类型——同一摘要按不同 mime 解码是不同编码。
#[test]
fn upload_alias_key_includes_media_type() {
    let budget = Budget::new(10, 2);
    let png = UploadId::from_payload("AAAA", "image/png");
    let jpeg = UploadId::from_payload("AAAA", "image/jpeg");
    assert_ne!(
        source_ref(&ImageSource::Uploaded(png), 0, budget, (10, 20)),
        source_ref(&ImageSource::Uploaded(jpeg), 0, budget, (10, 20)),
        "同一摘要不同媒体类型不能共用 alias"
    );
}

/// m5：字体/单元格尺寸变化后 alias 指向新 `cell_px` 的键，旧几何不可再被查到。
#[test]
fn cell_px_change_points_alias_at_new_key() {
    let mut cache = Cache {
        observed_cell_px: Some((10, 20)),
        ..Cache::default()
    };
    let old_key = key("a.png", 1, 10, 10, 2, (10, 20));
    let old_src = source("a.png");
    let id = spawned(cache.begin(old_src.clone(), old_key.clone()));
    assert_eq!(
        cache.complete(&old_key, id, Ok(ready_image())),
        CompleteOutcome::Updated
    );
    assert!(matches!(cache.lookup(old_src), Some(ImageState::Ready(_))));

    // 字体变大：新键在途时查表只看到 Pending，不会回落旧几何。
    let new_src = SourceRef::File {
        source: "a.png".to_string(),
        cols: 10,
        max_rows: 2,
        cell_px: (20, 40),
    };
    let new_key = key("a.png", 1, 10, 10, 2, (20, 40));
    let id = spawned(cache.begin(new_src.clone(), new_key.clone()));
    assert!(
        matches!(cache.lookup(new_src), Some(ImageState::Pending)),
        "新 cell_px 的键在途时不得返回旧几何"
    );
    assert_eq!(
        cache.complete(&new_key, id, Ok(ready_image())),
        CompleteOutcome::Updated
    );
}

/// m5：`lookup` 取 effect 边界最近观测到的 `cell_px`；同一图片在两种单元格尺寸
/// 下得到不同几何，变化后不会长期用旧值。
#[test]
#[serial_test::serial]
fn observed_cell_px_change_reencodes_geometry() {
    let (_dir, source_path) = png_file(200, 100);
    let budget = Budget::new(40, 6);
    assert!(request_until_ready(
        ImageSource::File(&source_path),
        budget,
        Duration::from_secs(10)
    ));
    let old = lookup(&ImageSource::File(&source_path), budget).expect("first ready");
    assert_eq!((old.cols, old.rows), (24, 6), "10×20 单元格的铺满几何");

    // 模拟字体放大：effect 边界观测到 20×40，请求换新键。
    request(
        ImageSource::File(&source_path),
        budget,
        &kitty_caps(),
        Some((20, 40)),
    );
    if let Some(placement) = lookup(&ImageSource::File(&source_path), budget) {
        assert_ne!(
            (placement.cols, placement.rows),
            (24, 6),
            "不得继续返回旧 cell_px 的几何"
        );
    }
    assert!(wait_ready(
        ImageSource::File(&source_path),
        budget,
        Duration::from_secs(10)
    ));
    let new = lookup(&ImageSource::File(&source_path), budget).expect("second ready");
    assert_eq!((new.cols, new.rows), (20, 5), "20×40 单元格的铺满几何");
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
