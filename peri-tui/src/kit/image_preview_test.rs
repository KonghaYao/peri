//! Tests for kit::image_preview（image-p0-p1-spec §6.5 验收断言）。
//!
//! TestBackend 不做终端解释，buffer cell 的 symbol 原样保留 escape 序列
//! （S2 §8）：首帧 kitty transmit 以 `\x1b_G` 开头，恰好 1 处（y==0 首行
//! cell，kitty.rs:180-184）；第二帧同 `&Protocol` 0 处、占位符 U+10EEEE 仍在
//! （AtomicBool 跨帧持久化，S2 §4）。

#[cfg(test)]
use super::*;
use image::{DynamicImage, ImageBuffer, Rgba};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};

/// 构造 w×h 纯色 RGBA 图片。
fn solid_image(w: u32, h: u32) -> DynamicImage {
    ImageBuffer::<Rgba<u8>, _>::from_pixel(w, h, Rgba([255, 0, 0, 255])).into()
}

/// 仅覆盖 graphics 字段的能力集合（其余默认全能力）。
fn caps(graphics: GraphicsProtocol) -> TerminalCaps {
    TerminalCaps {
        graphics,
        ..Default::default()
    }
}

/// 统计 buffer 中所有 cell symbol 里的 kitty transmit 序列数。
/// transmit 序列以 `\x1b_G` 开头；占位符序列只含 `\x1b[s` / `\x1b[38;2;..m` /
/// `\x1b[u`，不含 `_G`。
fn count_transmit(buf: &Buffer) -> usize {
    buf.content()
        .iter()
        .filter(|c| c.symbol().contains("\x1b_G"))
        .count()
}

/// 轮询后台 resize 线程直至结果应用；超时返回 false（正常环境 <100ms）。
fn wait_for_resize(async_img: &mut AsyncImage) -> bool {
    for _ in 0..200 {
        if async_img.poll_completed() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

// ── 协议门控（§6.5 第一组）───────────────────────────────────────────

#[test]
fn supported_gates_on_capability_bit() {
    assert!(supported(&caps(GraphicsProtocol::Kitty)));
    assert!(
        !supported(&caps(GraphicsProtocol::ITerm2)),
        "ITerm2 检测但 disabled：未验证不启用"
    );
    assert!(
        !supported(&TerminalCaps::default()),
        "None 安全默认：未探测/未知环境无像素渲染"
    );
}

#[test]
fn picker_for_maps_caps_to_protocol_type() {
    use ratatui_image::picker::ProtocolType;

    assert_eq!(
        picker_for(&caps(GraphicsProtocol::Kitty)).protocol_type(),
        ProtocolType::Kitty
    );
    // ITerm2 / None 不强制 Kitty（halfblocks 安全态，像素渲染由 supported 门控）。
    assert_ne!(
        picker_for(&caps(GraphicsProtocol::ITerm2)).protocol_type(),
        ProtocolType::Kitty
    );
    assert_ne!(
        picker_for(&TerminalCaps::default()).protocol_type(),
        ProtocolType::Kitty
    );
}

// ── static_protocol 跨帧 transmit（§6.5 第二组）───────────────────────

#[test]
fn static_protocol_transmit_once_across_frames() {
    let picker = picker_for(&caps(GraphicsProtocol::Kitty));
    let proto = static_protocol(
        &picker,
        solid_image(64, 64),
        Size::new(8, 8),
        Resize::Fit(None),
    )
    .expect("static protocol 构造失败");

    let mut term = Terminal::new(TestBackend::new(20, 12)).unwrap();
    let area = Rect::new(0, 0, 8, 8);

    // 首帧：transmit 恰好 1 次（整张图一个 transmit，写在 y==0 首行 cell）。
    term.draw(|f| f.render_widget(ratatui_image::Image::new(&proto), area))
        .unwrap();
    assert_eq!(count_transmit(term.backend().buffer()), 1);

    // 第二帧（同一 &Protocol，未重建）：transmit 不再出现，只有占位符。
    term.draw(|f| f.render_widget(ratatui_image::Image::new(&proto), area))
        .unwrap();
    assert_eq!(
        count_transmit(term.backend().buffer()),
        0,
        "第二帧不应再 transmit（AtomicBool 已置位）"
    );
    assert!(
        term.backend()
            .buffer()
            .content()
            .iter()
            .any(|c| c.symbol().contains('\u{10EEEE}')),
        "第二帧应仍有占位符渲染"
    );
}

// ── AsyncImage 后台重编码（§6.5 第三组）───────────────────────────────

#[test]
fn async_image_resizes_off_thread_and_renders() {
    let picker = picker_for(&caps(GraphicsProtocol::Kitty));
    let mut async_img = AsyncImage::new(&picker, solid_image(64, 64));
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 12));
    let area = Rect::new(0, 0, 8, 8);
    let resize = Resize::Fit(None);

    // 初始：协议未编码 → needs_resize 触发。
    assert!(
        async_img.needs_resize(&resize, area.into()).is_some(),
        "新构造的 AsyncImage 应需要首帧 resize"
    );

    // 渲染：非阻塞发出请求（该帧可能暂无编码帧）。
    async_img.render(area, &mut buf);

    // 后台线程完成 → 结果应用。
    assert!(
        wait_for_resize(&mut async_img),
        "后台 resize 应在超时内完成"
    );

    // 同尺寸：不再需要 resize。
    assert!(
        async_img.needs_resize(&resize, area.into()).is_none(),
        "已编码尺寸下不应重复 resize"
    );

    // 渲染：首帧 transmit 可见。
    async_img.render(area, &mut buf);
    assert_eq!(count_transmit(&buf), 1, "首帧渲染应恰好 transmit 一次");

    // 面积变化 → 再次触发 resize（尺寸变化帧重传整图，协议必需）。
    let smaller = Rect::new(0, 0, 4, 4);
    assert!(
        async_img.needs_resize(&resize, smaller.into()).is_some(),
        "面积变化应触发 resize"
    );
    async_img.render(smaller, &mut buf);
    assert!(
        wait_for_resize(&mut async_img),
        "面积变化后的后台 resize 应在超时内完成"
    );
    async_img.render(smaller, &mut buf);
    assert_eq!(count_transmit(&buf), 1, "尺寸变化后首帧重新 transmit 一次");
    assert!(
        async_img.needs_resize(&resize, smaller.into()).is_none(),
        "新尺寸下不再重复 resize"
    );
}

#[test]
fn async_image_poll_completed_reports_update() {
    let picker = picker_for(&caps(GraphicsProtocol::Kitty));
    let mut async_img = AsyncImage::new(&picker, solid_image(32, 32));
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 12));
    let area = Rect::new(0, 0, 8, 8);

    // 无请求发出时：无更新。
    assert!(!async_img.poll_completed());

    async_img.render(area, &mut buf);
    assert!(wait_for_resize(&mut async_img), "发出请求后应能取回更新");
    // 已取回：再次轮询无更新。
    assert!(!async_img.poll_completed());
}
