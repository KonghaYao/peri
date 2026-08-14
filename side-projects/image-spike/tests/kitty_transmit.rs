//! 最小验证（spike S2）：
//! 1. ratatui-image 的 Kitty escape 是否进入 ratatui buffer（TestBackend 可观察）？
//! 2. AtomicBool 机制：同一 Protocol 跨帧渲染时，transmit 序列只在首帧出现一次。
//!
//! 背景（源码证据）：
//! - `protocol/kitty.rs:42-50` `make_transmit()` 用 `transmitted.swap(true, SeqCst)`：
//!   首次返回 transmit 串，之后返回 None。
//! - `protocol/kitty.rs:81-87` `ProtocolTrait::render` 每帧调用 `make_transmit()`，
//!   首帧把 transmit 序列 + unicode-placeholder 写进首行 cell 的 symbol
//!   （kitty.rs:182-184, 212-214），其余 cell 设 `CellDiffOption::Skip`。
//! - 占位符消失时 kitty 自动移除 placement（kitty.rs:221-223 注释），无显式 delete API。

use image::{DynamicImage, ImageBuffer, Rgba};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::Terminal;
use ratatui_image::protocol::kitty::Kitty;
use ratatui_image::protocol::Protocol;
use ratatui_image::Image;

/// 构造 8x8 纯色 RGBA 图片的 Kitty Protocol（id 固定为 1，非 tmux）。
/// Kitty::new 直接构造可绕过 Picker（picker 的协议检测属于 peri TerminalCaps 侧职责）。
fn solid_protocol(w: u32, h: u32) -> Protocol {
    let img: DynamicImage =
        ImageBuffer::<Rgba<u8>, _>::from_pixel(w, h, Rgba([255, 0, 0, 255])).into();
    Protocol::Kitty(
        Kitty::new(img, Size::new(w as u16, h as u16), 1, false)
            .expect("kitty protocol 构造失败"),
    )
}

/// 统计 buffer 中所有 cell symbol 里的 kitty transmit 序列数。
/// transmit 序列以 `\x1b_G` 开头（kitty.rs:249-253 `{escape}_Gq=2,...`）；
/// 占位符序列只含 `\x1b[s`、`\x1b[38;2;..m`、`\x1b[u`，不含 `_G`。
fn count_transmit(buf: &Buffer) -> usize {
    buf.content()
        .iter()
        .filter(|c| c.symbol().contains("\x1b_G"))
        .count()
}

/// 收集非空 symbol 便于人工检查（debug 输出）。
fn non_empty_symbols(buf: &Buffer) -> Vec<String> {
    buf.content()
        .iter()
        .map(|c| c.symbol().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn kitty_escape_visible_in_test_backend_buffer() {
    let proto = solid_protocol(8, 8);
    let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();

    term.draw(|f| {
        f.render_widget(Image::new(&proto), Rect::new(0, 0, 8, 8));
    })
    .unwrap();

    let buf = term.backend().buffer();
    let symbols = non_empty_symbols(buf);

    // 关键断言 1：escape 确实进入了 ratatui buffer（TestBackend 不做终端解释，原样保留）。
    assert!(
        symbols.iter().any(|s| s.contains("\x1b_G")),
        "buffer 中应能观察到 kitty transmit escape（\\x1b_G...），实际符号：{symbols:#?}"
    );

    // 关键断言 2：transmit 只出现 1 次（整张图一个 transmit，写在 y==0 首行 cell）。
    assert_eq!(count_transmit(buf), 1, "首帧 transmit 应恰好出现一次");

    // 关键断言 3：占位符行数 = 图片行数（每行首 cell 含 U+10EEEE 占位符序列）。
    let placeholder_rows = symbols
        .iter()
        .filter(|s| s.contains('\u{10EEEE}'))
        .count();
    assert_eq!(placeholder_rows, 8, "8 行图片应有 8 个含占位符的 cell");

    // 关键断言 4：同一 cell 内 transmit + 占位符共存于首行首 cell。
    let first_row = symbols
        .iter()
        .find(|s| s.contains("\x1b_G"))
        .expect("首帧必有 transmit");
    assert!(first_row.contains('\u{10EEEE}'), "首行 cell 应同时含 transmit 与占位符");

    // 人工检查入口：cargo test -- --nocapture 可查看完整符号。
    eprintln!("首行首 cell symbol 长度 {} 字节", first_row.len());
}

#[test]
fn transmit_happens_only_on_first_frame() {
    let proto = solid_protocol(8, 8);
    let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
    let area = Rect::new(0, 0, 8, 8);

    // 第一帧：transmit 出现一次。
    term.draw(|f| f.render_widget(Image::new(&proto), area)).unwrap();
    assert_eq!(count_transmit(term.backend().buffer()), 1);

    // 第二帧（同一 &Protocol，未重建）：transmit 不再出现，只有占位符。
    term.draw(|f| f.render_widget(Image::new(&proto), area)).unwrap();
    assert_eq!(
        count_transmit(term.backend().buffer()),
        0,
        "第二帧不应再 transmit（AtomicBool 已置位）"
    );

    // 占位符仍在：图片持续可见。
    let symbols = non_empty_symbols(term.backend().buffer());
    assert!(
        symbols.iter().any(|s| s.contains('\u{10EEEE}')),
        "第二帧应仍有占位符渲染"
    );
}
