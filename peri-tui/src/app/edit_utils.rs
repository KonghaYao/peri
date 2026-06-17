use ratatui::{style::Style, text::Span};
use tui_textarea::TextArea;

use crate::ui::theme;

/// 确保光标在滚动视口内可见，返回调整后的 scroll_offset
pub fn ensure_cursor_visible(cursor_row: u16, scroll_offset: u16, visible_height: u16) -> u16 {
    if visible_height == 0 {
        return 0;
    }
    if cursor_row < scroll_offset {
        cursor_row
    } else if cursor_row >= scroll_offset + visible_height {
        cursor_row.saturating_sub(visible_height - 1)
    } else {
        scroll_offset
    }
}

pub fn build_textarea(disabled: bool) -> TextArea<'static> {
    build_textarea_with_hint(disabled, "")
}

fn build_textarea_with_hint(_disabled: bool, hint: &str) -> TextArea<'static> {
    let mut ta = TextArea::default();

    // 统一灰色边框
    let border_color = theme::MUTED;

    ta.set_cursor_line_style(Style::default());
    // Windows：禁用 textarea 自身的 REVERSED buffer 光标，改用终端光标（IME 候选窗跟随）。
    // macOS/Linux：保留默认 REVERSED buffer 光标，避免长行行尾光标不可见 bug
    // （cursor - (width-1) 推断与 sticky scroll 不一致）。
    // 详见 spec/issues/2026-06-17-main-textarea-cursor-invisible-long-line.md。
    #[cfg(target_os = "windows")]
    ta.set_cursor_style(Style::default());
    ta.set_style(Style::default().fg(theme::TEXT));
    let mut block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::TOP | ratatui::widgets::Borders::BOTTOM)
        .border_style(Style::default().fg(border_color))
        .padding(ratatui::widgets::Padding::new(2, 0, 0, 0));
    if !hint.is_empty() {
        block = block.title(Span::styled(
            hint.to_owned(),
            Style::default().fg(theme::MUTED),
        ));
    }
    ta.set_block(block);
    ta
}
