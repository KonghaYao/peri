//! 人工验证辅助：把 Kitty 首帧 escape 转储为十六进制输出。
//! 运行：cargo run --example dump_escape
//!
//! 目的：在无 kitty 终端环境也能确认 transmit 序列结构（a=T, U=1, f=32, t=d, m=1/m=0 分块）。

use image::{DynamicImage, ImageBuffer, Rgba};
use ratatui::backend::TestBackend;
use ratatui::layout::{Rect, Size};
use ratatui::Terminal;
use ratatui_image::protocol::kitty::Kitty;
use ratatui_image::protocol::Protocol;
use ratatui_image::Image;

fn main() {
    let img: DynamicImage =
        ImageBuffer::<Rgba<u8>, _>::from_pixel(8, 8, Rgba([255, 0, 0, 255])).into();
    let proto = Protocol::Kitty(
        Kitty::new(img, Size::new(8, 8), 1, false).expect("kitty proto"),
    );
    let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
    term.draw(|f| f.render_widget(Image::new(&proto), Rect::new(0, 0, 8, 8)))
        .unwrap();
    let buf = term.backend().buffer();
    let non_empty: Vec<usize> = buf
        .content()
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.symbol().is_empty())
        .map(|(i, _)| i)
        .collect();
    println!("非空 cell 索引: {non_empty:?}");
    let first = &buf.content()[non_empty[0]];
    println!("--- 首行首 cell symbol（{} 字节）---", first.symbol().len());
    for b in first.symbol().as_bytes() {
        print!("{b:02x} ");
    }
    println!();
}
