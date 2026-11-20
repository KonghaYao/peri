//! 无状态 `Spinner` 的帧选择、图标拼装与宿主文字样式回归。
//!
//! 关键约定：帧序号由宿主按时钟推进；组件只做 `frame % frames.len()` 与
//! 「图标 + 空格 + 宿主文字」拼装，不读时钟、不持有计时器。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::Spinner;

const FRAMES: &[char] = &['a', 'b', 'c'];

fn text(spans: Vec<Span<'static>>) -> Line<'static> {
    Line::from(spans)
}

fn rendered(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn glyph_wraps_the_external_frame_index() {
    assert_eq!(Spinner::new(FRAMES, 0).glyph(), Some('a'));
    assert_eq!(Spinner::new(FRAMES, 2).glyph(), Some('c'));
    assert_eq!(Spinner::new(FRAMES, 3).glyph(), Some('a'));
    assert_eq!(Spinner::new(FRAMES, 7).glyph(), Some('b'));
}

#[test]
fn glyph_is_absent_without_frames() {
    assert_eq!(Spinner::new(&[], 5).glyph(), None);
    let line = Spinner::new(&[], 5)
        .text(text(vec![Span::raw("待机")]))
        .line();
    assert_eq!(rendered(&line), "待机", "空帧集不补图标也不补空格");
}

#[test]
fn line_prepends_the_icon_and_one_space() {
    let line = Spinner::new(FRAMES, 1)
        .icon_style(Style::new().fg(Color::Cyan))
        .text(text(vec![
            Span::styled("思考中…", Style::new().fg(Color::Cyan)),
            Span::styled(" (12s)", Style::new().fg(Color::DarkGray)),
        ]))
        .line();

    assert_eq!(rendered(&line), "b 思考中… (12s)");
    assert_eq!(line.spans[0].content.as_ref(), "b ");
    assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
    // 宿主已格式化的文字原样保留（包括同一行内的不同颜色）。
    assert_eq!(line.spans[1].content.as_ref(), "思考中…");
    assert_eq!(line.spans[1].style.fg, Some(Color::Cyan));
    assert_eq!(line.spans[2].content.as_ref(), " (12s)");
    assert_eq!(line.spans[2].style.fg, Some(Color::DarkGray));
}

#[test]
fn line_without_text_is_just_the_icon() {
    let line = Spinner::new(FRAMES, 2).line();
    assert_eq!(rendered(&line), "c");
    assert_eq!(line.spans.len(), 1);
}

#[test]
fn fixed_input_renders_a_reproducible_row() {
    // Storybook `spinner/active` 的固定输入：帧 3、footer 帧集首帧起的第 4 个。
    let line = Spinner::new(&['✳', '✴', '✵', '✶'], 3)
        .icon_style(Style::new().fg(Color::Cyan))
        .text(text(vec![Span::styled(
            "思考中… (12s · ↓ 1.5k tokens)",
            Style::new().fg(Color::Cyan),
        )]))
        .line();
    let area = Rect::new(0, 0, 40, 1);
    let mut buf = Buffer::empty(area);
    Paragraph::new(line).render(area, &mut buf);
    let mut row = String::new();
    let mut skip = 0u16;
    for x in 0..40 {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let symbol = buf[(x, 0)].symbol();
        row.push_str(symbol);
        skip = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1) as u16;
    }
    assert_eq!(row.trim_end(), "✶ 思考中… (12s · ↓ 1.5k tokens)");
}
