//! 附件预览：列宽/高度策略、预留几何与绘制。
//!
//! 组件级用例经 `ratatui_kit::test_util::render_frames` 在 TestBackend 上渲染
//! 真实 `InputArea`：预览是否出现以 buffer 里的 Kitty 协议串/占位符为准，
//! 高度以 composer 顶边框所在行号为准。缓存状态机在 `image_view::cache` 内测试。

use std::time::{Duration, Instant};

use image::{DynamicImage, Rgba, RgbaImage};
use ratatui_kit::ratatui::buffer::Buffer;
use ratatui_kit::ratatui::layout::Rect;
use serial_test::serial;

use crate::kit::atoms::{INPUT_BUFFER, PENDING_ATTACHMENTS, PendingAttachment, TERMINAL_CAPS};
use crate::kit::input_area::InputAreaProps;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::terminal_caps::{GraphicsProtocol, TerminalCaps};

use super::*;

// ── 夹具 ──────────────────────────────────────────────────────────────────

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

/// 与粘贴路径一致：附件进 composer 的同时登记上传仓库。
fn push_attachment(media_type: &str, base64: String) {
    let attachment = PendingAttachment::image(media_type, base64);
    crate::kit::steer_state::retain_attachment_uploads(std::slice::from_ref(&attachment));
    PENDING_ATTACHMENTS.state().write().push(attachment);
}

fn reset_globals() {
    crate::kit::atoms::init_atoms();
    crate::i18n::init(Some("en"));
    PENDING_ATTACHMENTS.state().write().clear();
    INPUT_BUFFER.state().write().clear();
    crate::kit::image_view::release_all_uploads();
}

/// 测试期间强制能力位；Drop 还原（避免污染其它用例）。
struct CapsGuard(TerminalCaps);

impl CapsGuard {
    fn new(graphics: GraphicsProtocol) -> Self {
        let saved = *TERMINAL_CAPS.state().read();
        TERMINAL_CAPS.set(TerminalCaps { graphics, ..saved });
        Self(saved)
    }

    fn kitty() -> Self {
        Self::new(GraphicsProtocol::Kitty)
    }
}

impl Drop for CapsGuard {
    fn drop(&mut self) {
        TERMINAL_CAPS.set(self.0);
    }
}

// ── 布局策略 ──────────────────────────────────────────────────────────────

#[test]
fn preview_cols_uses_full_composer_width() {
    assert_eq!(preview_cols(80), 78, "宽 composer 不再被 40 列上限截断");
    assert_eq!(preview_cols(20), 18);
    assert_eq!(preview_cols(9), 0, "窄屏不预览");
    assert_eq!(preview_cols(0), 0);
}

#[test]
fn layout_height_yields_to_budget() {
    assert_eq!(layout_height(None, 3, PREVIEW_ROWS), PREVIEW_ROWS);
    assert_eq!(layout_height(Some(9), 3, PREVIEW_ROWS), PREVIEW_ROWS);
    assert_eq!(
        layout_height(Some(8), 3, PREVIEW_ROWS),
        0,
        "预算不足时整块让位，不挤压 transcript"
    );
    assert_eq!(layout_height(Some(9), 3, 3), 3, "Ready 后按实际行数收缩");
    assert_eq!(layout_height(None, 3, 0), 0, "失败/无图不预留");
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

// ── 组件级：预留高度 / 绘制 / 移除 ────────────────────────────────────────

fn render(height: u16, frames: usize) -> Buffer {
    let props = InputAreaProps {
        grid: GridSpec::grid_for(80),
        ..InputAreaProps::default()
    };
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
    push_attachment("image/png", png_base64(64, 64));

    let buf = render(25, 1);
    let preview = Rect::new(0, 0, 80, PREVIEW_ROWS);
    if has_protocol_marker(&buf, preview) {
        // 同一帧快照内的自洽断言：body 在帧首按缓存定预留高度，绘制钩子在同帧
        // 稍后才查缓存；编码恰在两者之间完成时会「已画标记、高度仍按 Pending
        // 预留」。已绘制时只要求高度落在预留范围内，避免与编码线程竞速。
        let top = composer_top(&buf);
        assert!(
            top > 0 && top <= PREVIEW_ROWS,
            "已绘制预览时高度必须在预留范围内，实际 {top}"
        );
    } else {
        assert_eq!(composer_top(&buf), PREVIEW_ROWS, "待编码时预留最大预览高度");
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = buf;
    while !has_protocol_marker(&buf, preview) {
        assert!(Instant::now() < deadline, "编码完成后预览应出现在预留区");
        std::thread::sleep(Duration::from_millis(10));
        buf = render(25, 1);
    }
    let top = composer_top(&buf);
    assert!(
        top > 0 && top <= PREVIEW_ROWS,
        "预览出现在预留区内，实际高度 {top}"
    );
}

#[test]
#[serial]
fn ready_preview_shrinks_reserved_height() {
    reset_globals();
    let _caps = CapsGuard::kitty();
    // 超宽图：按行数上限铺满后行数小于 PREVIEW_ROWS，Ready 后预留高度收缩。
    push_attachment("image/png", png_base64(1600, 200));

    let preview = Rect::new(0, 0, 80, PREVIEW_ROWS);
    let appearance_deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = render(25, 1);
    while !has_protocol_marker(&buf, preview) {
        assert!(
            Instant::now() < appearance_deadline,
            "预览在 10s 内从未出现：解码/绘制未完成，无法验证高度收缩"
        );
        std::thread::sleep(Duration::from_millis(10));
        buf = render(25, 1);
    }

    // 高度收缩不保证与协议标记同帧：`use_preview` 在帧首按缓存定预留高度，
    // 绘制钩子在同帧稍后才查缓存；解码回调恰好落在两者之间时，同一帧会
    // 「已画预览标记、高度仍按 Pending 预留」。按有界收敛断言：反复 render
    // 直到 composer 顶边框落在 (0, PREVIEW_ROWS)，超时才判定未收缩。
    let shrink_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let top = composer_top(&buf);
        if top > 0 && top < PREVIEW_ROWS {
            break;
        }
        assert!(
            Instant::now() < shrink_deadline,
            "预览已出现，但高度在 5s 内未收敛：composer 顶部仍在第 {top} 行（应落在 1..{PREVIEW_ROWS}）"
        );
        std::thread::sleep(Duration::from_millis(10));
        buf = render(25, 1);
    }
}

#[test]
#[serial]
fn removing_attachment_restores_height_and_clears_pixels() {
    reset_globals();
    let _caps = CapsGuard::kitty();
    push_attachment("image/png", png_base64(32, 32));
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
        for (w, h) in [(16, 16), (24, 24)] {
            attachments.push(PendingAttachment::image("image/png", png_base64(w, h)));
        }
    }
    crate::kit::steer_state::retain_attachment_uploads(&PENDING_ATTACHMENTS.state().read());
    let buf = render(25, 2);
    assert!(composer_top(&buf) <= PREVIEW_ROWS, "预览高度不超过最大预留");
    let footer = buffer_text(&buf, composer_top(&buf) + 2);
    assert!(
        footer.contains("[Image #1]") && footer.contains("[Image #2]"),
        "底部标签仍列出全部附件：{footer}"
    );
}

#[test]
#[serial]
fn unsupported_protocol_keeps_text_only() {
    reset_globals();
    let _caps = CapsGuard::new(GraphicsProtocol::None);
    push_attachment("image/png", png_base64(64, 64));
    let buf = render(25, 2);
    assert_eq!(composer_top(&buf), 0, "无协议时不预留像素区");
    assert!(!buffer_has_protocol_marker(&buf), "不得写入任何图片协议");
}

/// M2 回归：并发解码槽位满时预览请求被 Deferred；槽位释放（bump revision）后
/// effect 必须重试并最终 Ready。
///
/// 关键断言在**同一次** `render_frames(frames = 2)` 内：持久组件树的 effect 只在
/// deps 变化时重跑，只有 deps 含 `image_view::revision()` 才会在第二帧重试——
/// 否则缓存无条目（Request 从未登记），预览会一直停在文本降级。
/// 登记与编码是两条并行时间线：第二帧登记后 64×64 图可能瞬间 Ready，因此这里
/// 只断言「已登记」（Pending 或 Ready），不断言瞬态 Pending 与预留高度。
#[test]
#[serial]
fn deferred_preview_retries_after_slot_release() {
    reset_globals();
    let _caps = CapsGuard::kitty();
    push_attachment("image/png", png_base64(64, 64));
    let budget = preview_budget(preview_cols(80));
    let source = ImageSource::Uploaded(
        target(&PENDING_ATTACHMENTS.state().read())
            .expect("图片附件")
            .upload_id(),
    );

    // 第一帧请求被暂缓，并在同一帧内模拟槽位释放（bump revision）。
    crate::kit::image_view::defer_once_and_signal_release_for_test();
    let _ = render(25, 2);
    assert!(
        crate::kit::image_view::has_registered(source, budget),
        "revision 变化必须让 effect 在下一帧重试请求：缓存无条目说明 deps 缺 revision() 或 effect 未在第二帧重跑"
    );

    // 最终 Ready，并画出预览。
    assert!(
        crate::kit::image_view::wait_ready(source, budget, Duration::from_secs(10)),
        "重试后的编码应最终 Ready"
    );
    let preview = Rect::new(0, 0, 80, PREVIEW_ROWS);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = render(25, 1);
    while !has_protocol_marker(&buf, preview) {
        assert!(Instant::now() < deadline, "Ready 后预览应出现");
        std::thread::sleep(Duration::from_millis(10));
        buf = render(25, 1);
    }
    crate::kit::image_view::release_all_uploads();
}

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
