//! 附件预览：缓存状态机、缓存键与 composer 预留高度/绘制。
//!
//! 组件级用例经 `ratatui_kit::test_util::render_frames` 在 TestBackend 上渲染
//! 真实 `InputArea`：预览是否出现以 buffer 里的 Kitty 协议串/占位符为准，
//! 高度以 composer 顶边框所在行号为准。

use std::sync::Arc;
use std::time::{Duration, Instant};

use image::{DynamicImage, Rgba, RgbaImage};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::{Constraint, Direction, Rect};
use serial_test::serial;

use crate::kit::atoms::{INPUT_BUFFER, PENDING_ATTACHMENTS, PendingAttachment, TERMINAL_CAPS};
use crate::kit::image_preview::{InlineImage, InlineImageEncoder};
use crate::kit::input_area::InputAreaProps;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::terminal_caps::{GraphicsProtocol, TerminalCaps};

use super::*;

// ── 夹具 ──────────────────────────────────────────────────────────────────

fn attachment(base64: &str) -> PendingAttachment {
    PendingAttachment::image("image/png", base64)
}

fn preview_key(base64: &str, cols: u16, rows: u16) -> PreviewKey {
    PreviewKey::new(&attachment(base64), cols, rows)
}

fn kitty_caps() -> TerminalCaps {
    TerminalCaps {
        graphics: GraphicsProtocol::Kitty,
        ..TerminalCaps::default()
    }
}

fn encoder() -> InlineImageEncoder {
    InlineImageEncoder::for_caps(&kitty_caps(), None).expect("Kitty 应支持协议")
}

fn ready_image() -> Arc<InlineImage> {
    let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([220, 30, 30, 255])));
    Arc::new(
        encoder()
            .encode(img, 8, 4)
            .expect("小图应能在单元格预算内编码"),
    )
}

/// RGBA 纯色图 → PNG → 标准 base64（与粘贴附件同一载荷格式）。
fn png_base64(width: u32, height: u32) -> String {
    use base64::Engine as _;
    let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(
        width,
        height,
        Rgba([30, 120, 200, 255]),
    ));
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .expect("测试图片应可编码为 PNG");
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn wait_ready(key: &PreviewKey, timeout: Duration) -> Option<Arc<InlineImage>> {
    let deadline = Instant::now() + timeout;
    loop {
        match state_of(key) {
            Some(PreviewState::Ready(image)) => return Some(image),
            Some(PreviewState::Failed(reason)) => panic!("预览编码失败：{reason}"),
            Some(PreviewState::Pending) | None => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn reset_globals() {
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    PENDING_ATTACHMENTS.state().write().clear();
    INPUT_BUFFER.state().write().clear();
    cache().lock().clear();
}

/// 测试期间强制 Kitty 能力位；Drop 还原（避免污染其它用例）。
struct CapsGuard(TerminalCaps);

impl CapsGuard {
    fn kitty() -> Self {
        let saved = *TERMINAL_CAPS.state().read();
        TERMINAL_CAPS.set(TerminalCaps {
            graphics: GraphicsProtocol::Kitty,
            ..saved
        });
        Self(saved)
    }
}

impl Drop for CapsGuard {
    fn drop(&mut self) {
        TERMINAL_CAPS.set(self.0);
    }
}

// ── 缓存键 / 布局 ─────────────────────────────────────────────────────────

#[test]
fn key_distinguishes_content_media_and_target_size() {
    let base = preview_key("AAAA", 40, 6);
    assert_eq!(
        base,
        preview_key("AAAA", 40, 6),
        "同一内容同一目标应命中缓存"
    );
    assert_ne!(base, preview_key("BBBB", 40, 6), "内容变化必须换键");
    assert_ne!(base, preview_key("AAAA", 30, 6), "目标列宽变化必须换键");
    assert_ne!(base, preview_key("AAAA", 40, 5), "目标行高变化必须换键");

    let jpeg = PreviewKey::new(&PendingAttachment::image("image/jpeg", "AAAA"), 40, 6);
    assert_ne!(base, jpeg, "media_type 变化必须换键");
}

#[test]
fn memo_hashes_once_per_attachment_and_resets_on_removal() {
    let mut memo = PreviewKeyMemo::default();
    let first_attachment = attachment("AAAA");
    let first = memo
        .key_for(Some(&first_attachment), 1, 40, 6)
        .expect("图片附件应有缓存键");
    let again = memo
        .key_for(Some(&first_attachment), 1, 40, 6)
        .expect("同一附件应复用缓存键");
    assert!(Arc::ptr_eq(&first, &again), "同一附件不得重复哈希");

    let second_attachment = attachment("BBBB");
    let second = memo
        .key_for(Some(&second_attachment), 1, 40, 6)
        .expect("换附件后应重新计算");
    assert_ne!(first, second);
    assert!(
        memo.key_for(None, 0, 40, 6).is_none(),
        "附件清空（移除/提交）后不得残留旧键"
    );

    let restored = memo
        .key_for(Some(&first_attachment), 1, 40, 6)
        .expect("重新粘贴应重新计算键");
    assert_eq!(*restored, *first, "同一内容重新计算应得到同一身份");
}

#[test]
fn preview_cols_clamps_to_content_width() {
    assert_eq!(preview_cols(80), 40, "宽 composer 也最多按 40 列编码");
    assert_eq!(preview_cols(20), 18);
    assert_eq!(preview_cols(9), 0, "窄屏不预览");
    assert_eq!(preview_cols(0), 0);
}

#[test]
fn layout_height_yields_to_budget() {
    assert_eq!(layout_height(None, 3, true), PREVIEW_ROWS);
    assert_eq!(layout_height(Some(9), 3, true), PREVIEW_ROWS);
    assert_eq!(
        layout_height(Some(8), 3, true),
        0,
        "预算不足时整块让位，不挤压 transcript"
    );
    assert_eq!(layout_height(None, 3, false), 0);
}

#[test]
fn preview_rect_sits_directly_above_composer() {
    let area = Rect::new(2, 5, 40, 20);
    assert_eq!(
        preview_rect(area, 3, PREVIEW_ROWS),
        Some(Rect::new(2, 8, 40, PREVIEW_ROWS)),
        "预览 = 输入区顶 + 弹出层 + 队列，紧贴 composer 上沿"
    );
    assert_eq!(preview_rect(area, 16, PREVIEW_ROWS), None, "越界不画");
    assert_eq!(preview_rect(area, 3, 0), None, "无预览高度");
}

// ── 缓存状态机 ────────────────────────────────────────────────────────────

#[test]
fn attachment_change_invalidates_previous_result() {
    let mut cache = Cache::default();
    let first = preview_key("AAAA", 40, 6);
    let second = preview_key("BBBB", 40, 6);

    let id = cache.begin(first.clone()).expect("首个请求应登记");
    assert!(matches!(cache.state(&first), Some(PreviewState::Pending)));
    assert!(cache.complete(&first, id, Ok(ready_image())));
    assert!(matches!(cache.state(&first), Some(PreviewState::Ready(_))));

    let next = cache.begin(second.clone()).expect("换附件应重新登记");
    assert!(cache.state(&first).is_none(), "旧附件结果必须失效");
    assert!(matches!(cache.state(&second), Some(PreviewState::Pending)));
    let after_first = cache.revision;
    assert!(
        !cache.complete(&first, id, Ok(ready_image())),
        "旧请求结果不得写回新槽位"
    );
    assert_eq!(cache.revision, after_first, "被丢弃的结果不得递增版本");
    assert!(cache.complete(&second, next, Ok(ready_image())));
    assert_eq!(
        cache.revision,
        after_first + 1,
        "只有真正写回结果才递增版本"
    );
}

#[test]
fn stale_completion_is_ignored() {
    let mut cache = Cache::default();
    let key = preview_key("AAAA", 40, 6);
    let id = cache.begin(key.clone()).expect("请求应登记");
    assert!(!cache.complete(&key, id + 1, Ok(ready_image())));
    assert!(matches!(cache.state(&key), Some(PreviewState::Pending)));
    assert_eq!(cache.revision, 0);
}

#[test]
fn failed_result_falls_back_and_same_key_is_not_retried() {
    let mut cache = Cache::default();
    let key = preview_key("!!!", 40, 6);
    let id = cache.begin(key.clone()).expect("请求应登记");
    assert!(cache.complete(&key, id, Err("base64")));
    assert!(matches!(
        cache.state(&key),
        Some(PreviewState::Failed("base64"))
    ));
    assert!(
        cache.begin(key.clone()).is_none(),
        "同一附件失败后不反复起线程"
    );

    let next = preview_key("????", 40, 6);
    assert!(cache.begin(next).is_some(), "换附件后允许重新请求（重试）");
}

#[test]
fn clear_drops_slot_so_repeated_attachment_can_retry() {
    let mut cache = Cache::default();
    let key = preview_key("AAAA", 40, 6);
    let id = cache.begin(key.clone()).expect("请求应登记");
    assert!(cache.complete(&key, id, Err("format")));
    cache.clear();
    assert!(cache.state(&key).is_none());
    assert!(cache.begin(key).is_some(), "清空后同一附件可重试");
}

#[test]
#[serial]
fn unsupported_protocol_does_not_request() {
    reset_globals();
    let att = attachment("AAAA");
    let key = preview_key("AAAA", 40, 6);
    let before = revision();

    // None（未探测）与 ITerm2 都不得进入编码路径。
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
        request(Some(&att), Some(&key), 40, 6, &caps, None);
        assert!(state_of(&key).is_none(), "{caps:?} 不应登记缓存");
    }
    assert_eq!(revision(), before, "未请求不得走完成路径");
}

#[test]
#[serial]
fn request_decodes_base64_and_encodes_offscreen() {
    reset_globals();
    let att = attachment(&png_base64(64, 48));
    let key = preview_key(&att.base64_data, 40, PREVIEW_ROWS);
    let before = revision();

    request(
        Some(&att),
        Some(&key),
        40,
        PREVIEW_ROWS,
        &kitty_caps(),
        None,
    );
    let image = wait_ready(&key, Duration::from_secs(10)).expect("编码应在超时内完成");
    assert!(image.cols() <= 40, "编码宽度不得超过目标预算");
    assert!(image.rows() <= PREVIEW_ROWS, "编码高度不得超过预览区");
    assert!(revision() > before, "完成后必须递增缓存版本");
}

// ── 组件级：预留高度 / 绘制 / 移除 ────────────────────────────────────────

fn render(height: u16, frames: usize) -> Buffer {
    render_input(height, frames, |area| area)
}

fn render_input(
    height: u16,
    frames: usize,
    props: impl FnOnce(InputAreaProps) -> InputAreaProps,
) -> Buffer {
    let props = props(InputAreaProps {
        grid: GridSpec::grid_for(80),
        ..InputAreaProps::default()
    });
    ratatui_kit::test_util::render_frames(
        element!(crate::kit::input_area::InputArea(..props)),
        80,
        height,
        frames,
    )
}

/// composer 顶边框所在行 = 上方被弹出层/队列/预览占掉的行数。
fn composer_top(buf: &Buffer) -> u16 {
    let area = buf.area;
    (area.y..area.bottom())
        .find(|&y| (area.x..area.right()).any(|x| buf[(x, y)].symbol().contains('─')))
        .expect("composer 顶边框应可见")
}

fn has_protocol_marker(buf: &Buffer, rect: Rect) -> bool {
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let symbol = buf[(x, y)].symbol();
            if symbol.contains("\x1b_G")
                || symbol.contains('\u{10EEEE}')
                || symbol.contains("\x1bP")
            {
                return true;
            }
        }
    }
    false
}

fn buffer_has_protocol_marker(buf: &Buffer) -> bool {
    has_protocol_marker(buf, buf.area)
}

#[test]
#[serial]
fn no_attachment_keeps_height() {
    reset_globals();
    let _caps = CapsGuard::kitty();
    let buf = render(25, 2);
    assert_eq!(composer_top(&buf), 0, "无附件时 composer 顶边框在首行");
    assert!(!buffer_has_protocol_marker(&buf));
}

#[test]
#[serial]
fn image_attachment_reserves_preview_rows_and_draws_kitty() {
    reset_globals();
    let _caps = CapsGuard::kitty();
    PENDING_ATTACHMENTS
        .state()
        .write()
        .push(PendingAttachment::image("image/png", png_base64(64, 64)));

    let buf = render(25, 1);
    assert_eq!(
        composer_top(&buf),
        PREVIEW_ROWS,
        "有图片附件时 composer 上方预留 6 行"
    );

    let preview = Rect::new(0, 0, 80, PREVIEW_ROWS);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = buf;
    while !has_protocol_marker(&buf, preview) {
        assert!(Instant::now() < deadline, "编码完成后预览应出现在预留区");
        std::thread::sleep(Duration::from_millis(10));
        buf = render(25, 1);
    }
    assert!(
        has_protocol_marker(&buf, preview),
        "Kitty 协议串/占位符必须落在预留区内"
    );
    assert_eq!(composer_top(&buf), PREVIEW_ROWS, "预览出现不改变预留高度");
}

#[test]
#[serial]
fn removing_attachment_restores_height_and_clears_pixels() {
    reset_globals();
    let _caps = CapsGuard::kitty();
    PENDING_ATTACHMENTS
        .state()
        .write()
        .push(PendingAttachment::image("image/png", png_base64(32, 32)));
    let preview = Rect::new(0, 0, 80, PREVIEW_ROWS);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = render(25, 1);
    while !has_protocol_marker(&buf, preview) {
        assert!(Instant::now() < deadline, "移除前应已出现预览");
        std::thread::sleep(Duration::from_millis(10));
        buf = render(25, 1);
    }

    PENDING_ATTACHMENTS.state().write().clear();
    let buf = render(25, 2);
    assert_eq!(composer_top(&buf), 0, "移除附件后高度恢复");
    assert!(
        !buffer_has_protocol_marker(&buf),
        "移除附件后不得残留协议串/占位符"
    );
}

#[test]
#[serial]
fn multiple_images_preview_last_but_label_lists_all() {
    reset_globals();
    let _caps = CapsGuard::kitty();
    {
        let atom = PENDING_ATTACHMENTS.state();
        let mut attachments = atom.write();
        attachments.push(PendingAttachment::image("image/png", png_base64(16, 16)));
        attachments.push(PendingAttachment::image("image/png", png_base64(24, 24)));
    }
    let buf = render(25, 2);
    assert_eq!(composer_top(&buf), PREVIEW_ROWS);
    let footer = buffer_text(&buf, composer_top(&buf) + 2);
    assert!(
        footer.contains("[Image #1]") && footer.contains("[Image #2]"),
        "底部标签仍列出全部附件：{footer}"
    );
}

/// 单行可见文本：跳过双宽字符的续接 cell（symbol 为空），否则 CJK 会被拆成
/// 「排 队 一」。
/// 单行可见文本：按字符显示宽度推进，跳过双宽字符的续接 cell（否则 CJK 会被
/// 拆成「排 队 一」）。
fn buffer_text(buf: &Buffer, y: u16) -> String {
    use unicode_width::UnicodeWidthChar as _;
    let area = buf.area;
    let mut out = String::new();
    let mut x = area.x;
    while x < area.right() {
        let symbol = buf[(x, y)].symbol();
        let Some(ch) = symbol.chars().next() else {
            x = x.saturating_add(1);
            continue;
        };
        out.push(ch);
        x = x.saturating_add(u16::try_from(ch.width().unwrap_or(1).max(1)).unwrap_or(1));
    }
    out
}

#[test]
#[serial]
fn preview_sits_below_queue_and_above_composer() {
    reset_globals();
    let _caps = CapsGuard::kitty();
    INPUT_BUFFER
        .state()
        .write()
        .push_back(crate::kit::atoms::BufferedInput::text("排队一"));
    PENDING_ATTACHMENTS
        .state()
        .write()
        .push(PendingAttachment::image("image/png", png_base64(16, 16)));

    let buf = render(25, 1);
    assert_eq!(
        composer_top(&buf),
        1 + PREVIEW_ROWS,
        "队列 1 行 + 预览 6 行：预览在队列与 composer 之间"
    );
    assert!(buffer_text(&buf, 0).contains("排队一"), "队列行仍在最上方");
}

#[test]
#[serial]
fn budget_and_degraded_surfaces_skip_preview() {
    reset_globals();
    let _caps = CapsGuard::kitty();
    PENDING_ATTACHMENTS
        .state()
        .write()
        .push(PendingAttachment::image("image/png", png_base64(32, 32)));

    let tight = render_input(10, 2, |props| InputAreaProps {
        max_total_height: Some(8),
        ..props
    });
    assert_eq!(composer_top(&tight), 0, "预算不足时不预留预览");

    let degraded = render_input(10, 2, |props| InputAreaProps {
        max_lines: Some(2),
        ..props
    });
    assert_eq!(composer_top(&degraded), 0, "高度降级档（h<8）不预览");

    let hidden = render_input(10, 2, |props| InputAreaProps {
        hidden: true,
        ..props
    });
    assert!(
        !buffer_has_protocol_marker(&hidden),
        "面板打开（hidden）时不画预览"
    );

    // 窄屏：横向布局把 composer 压到 7 列 → 预览列宽不足，不预留。
    let narrow = ratatui_kit::test_util::render_frames(
        element!(
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                View(
                    width: Constraint::Length(7),
                    height: Constraint::Fill(1),
                ) {
                    crate::kit::input_area::InputArea(grid: GridSpec::grid_for(7))
                }
            }
        ),
        80,
        12,
        2,
    );
    assert_eq!(composer_top(&narrow), 0, "窄屏不预留预览");
    assert!(!buffer_has_protocol_marker(&narrow));
}

// ── 绘制钩子：协议无关（Sixel DCS）────────────────────────────────────────

#[test]
#[serial]
fn draw_hook_writes_sixel_dcs_into_preview_rect() {
    reset_globals();
    // TestBackend 拿不到单元格像素尺寸（Sixel 布局门控会关闭），这里直接验证
    // 绘制路径：注入已编码的 Sixel 协议后，预留区内必须出现 DCS 序列。
    let sixel_caps = TerminalCaps {
        graphics: GraphicsProtocol::Sixel,
        ..TerminalCaps::default()
    };
    let encoder = InlineImageEncoder::for_caps(&sixel_caps, Some((8, 16)))
        .expect("有单元格尺寸时 Sixel 可用");
    let image = Arc::new(
        encoder
            .encode(
                DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([10, 200, 10, 255]))),
                8,
                6,
            )
            .expect("Sixel 编码应成功"),
    );
    let key = preview_key("SIXEL", 40, PREVIEW_ROWS);
    {
        let mut cache = cache().lock();
        let id = cache.begin(key.clone()).expect("注入请求");
        assert!(cache.complete(&key, id, Ok(image)));
    }

    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    let frame = PreviewFrame {
        height: PREVIEW_ROWS,
        y_offset: 0,
        key: Some(Arc::new(key)),
    };
    frame.draw(&mut buf, area);
    assert!(
        has_protocol_marker(&buf, Rect::new(0, 0, 40, PREVIEW_ROWS)),
        "Sixel DCS 应写入预留区"
    );
}

#[test]
#[serial]
fn draw_hook_skips_pending_and_out_of_bounds_frames() {
    reset_globals();
    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    let key = preview_key("PENDING", 40, PREVIEW_ROWS);
    {
        let mut cache = cache().lock();
        let _ = cache.begin(key.clone()).expect("登记 Pending");
    }

    // Pending：不画。
    let pending = PreviewFrame {
        height: PREVIEW_ROWS,
        y_offset: 0,
        key: Some(Arc::new(key.clone())),
    };
    pending.draw(&mut buf, area);
    assert!(!buffer_has_protocol_marker(&buf));

    // 预留区越出输入区：不画（防止把图片覆盖到 composer/transcript 上）。
    let out_of_bounds = PreviewFrame {
        height: PREVIEW_ROWS,
        y_offset: 10,
        key: Some(Arc::new(key)),
    };
    out_of_bounds.draw(&mut buf, area);
    assert!(!buffer_has_protocol_marker(&buf));
}
