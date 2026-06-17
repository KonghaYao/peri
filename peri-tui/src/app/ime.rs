//! IME composition window positioning (Windows-only).
//!
//! On Windows terminal emulators, the IME composition window position is
//! determined by the terminal cursor position. If the terminal cursor stays at
//! (0, 0) — the top-left corner — the IME candidate window appears there
//! instead of following the text input box.
//!
//! This module calculates the textarea cursor's terminal-coordinate position.
//! The render loop calls `Frame::set_cursor` with this position so the terminal
//! knows where to anchor the IME composition window.
//!
//! ## Why Windows-only
//!
//! macOS and Linux terminal emulators generally handle IME composition window
//! positioning adequately without explicit cursor hints. Enabling this module
//! on those platforms would disable tui-textarea's default REVERSED buffer
//! cursor (to avoid double-cursor visual) and rely on the inferred terminal
//! cursor coordinates — but the inference formula (`cursor - (width - 1)`)
//! diverges from tui-textarea's sticky-scroll behavior on long lines, causing
//! the cursor to disappear at end of long lines (see
//! spec/issues/2026-06-17-main-textarea-cursor-invisible-long-line.md).
//!
//! Windows users accept this trade-off because IME candidate tracking is a
//! stronger UX requirement there. On macOS/Linux the REVERSED buffer cursor is
//! used directly with no terminal-cursor dependency.
#![cfg(target_os = "windows")]

use ratatui::layout::Rect;
use tui_textarea::TextArea;

/// `tui-textarea` 默认 tab 宽度。tab 字符按 tab stop 对齐到下一个 4 的倍数列。
const TAB_LEN: usize = 4;

/// 计算给定字符串前 `char_count` 个字符占据的显示列数。
///
/// 与 `tui-textarea` 的渲染逻辑对齐：tab 字符按 tab stop 对齐，CJK 字符按
/// `unicode-width` 计算（中文/日文/韩文占 2 列）。
fn display_width_before(s: &str, char_count: usize) -> usize {
    let mut col = 0usize;
    for c in s.chars().take(char_count) {
        if c == '\t' {
            // 跳到下一个 tab stop（与 tui-textarea wrap.rs::display_width_to 一致）
            col += TAB_LEN - (col % TAB_LEN);
        } else {
            col += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    col
}

/// Calculate the terminal-grid position of the visible textarea cursor.
///
/// Returns `None` if the textarea has zero visible area.
pub fn textarea_cursor_pos(textarea: &TextArea, textarea_area: Rect) -> Option<(u16, u16)> {
    let visible_height = textarea_area.height as usize;
    let visible_width = textarea_area.width as usize;
    if visible_height == 0 || visible_width == 0 {
        return None;
    }

    let (cursor_row, cursor_col) = textarea.cursor();

    // Vertical scroll: cursor is always kept within the visible area
    let scroll_row = cursor_row.saturating_sub(visible_height.saturating_sub(1));
    let visible_row = cursor_row.saturating_sub(scroll_row);

    // Horizontal scroll: infer scroll offset from cursor position.
    // tui-textarea keeps cursor within the visible area, so the leftmost scrolled
    // column is cursor_col - (visible_width - 1).
    let cursor_line = textarea
        .lines()
        .get(cursor_row)
        .map(|s| s.as_str())
        .unwrap_or("");
    let cursor_display_col = display_width_before(cursor_line, cursor_col);
    let scroll_col = cursor_display_col.saturating_sub(visible_width.saturating_sub(1));
    // Clamp to visible area: cursor must not be placed outside the inner rect.
    // Otherwise terminal emulators may ignore the cursor move, leaving a "ghost"
    // at the previous position — seen as residual cursor after deletion.
    let visible_col = cursor_display_col
        .saturating_sub(scroll_col)
        .min(visible_width.saturating_sub(1));

    // 使用 saturating_add 防御 u16 溢出（实际终端尺寸远小于 u16 上限，
    // 但作为坐标计算 API 加 saturating 保护更稳健）
    Some((
        textarea_area.x.saturating_add(visible_col as u16),
        textarea_area.y.saturating_add(visible_row as u16),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_pos_empty_textarea() {
        let ta = TextArea::default();
        // 0-height area should return None
        assert!(textarea_cursor_pos(&ta, Rect::new(0, 0, 80, 0)).is_none());
        // 0-width area should return None
        assert!(textarea_cursor_pos(&ta, Rect::new(0, 0, 0, 24)).is_none());
    }

    #[test]
    fn test_cursor_pos_top_left() {
        let mut ta = TextArea::default();
        ta.insert_str("hello");
        ta.move_cursor(tui_textarea::CursorMove::Jump(0, 0));
        // Cursor at (0, 0), textarea at (5, 10)
        let pos = textarea_cursor_pos(&ta, Rect::new(5, 10, 80, 24));
        assert_eq!(pos, Some((5, 10)));
    }

    #[test]
    fn test_cursor_pos_after_text() {
        let mut ta = TextArea::default();
        ta.insert_str("hi");
        // Cursor at (0, 2) after "hi"
        let pos = textarea_cursor_pos(&ta, Rect::new(0, 0, 80, 24));
        assert_eq!(pos, Some((2, 0)));
    }

    #[test]
    fn test_cursor_pos_with_cjk() {
        let mut ta = TextArea::default();
        ta.insert_str("你好");
        // Cursor at (0, 2 chars) which is display column 4
        let pos = textarea_cursor_pos(&ta, Rect::new(0, 10, 80, 24));
        assert_eq!(pos, Some((4, 10)));
    }

    #[test]
    fn test_cursor_pos_scroll_below_viewport() {
        let mut ta = TextArea::default();
        for _ in 0..30 {
            ta.insert_str("line\n");
        }
        // cursor_row=30, visible_height=24: scroll_row=30-23=7, visible_row=30-7=23
        let pos = textarea_cursor_pos(&ta, Rect::new(3, 5, 80, 24));
        assert_eq!(pos, Some((3, 5 + 23)));
    }

    #[test]
    fn test_cursor_pos_horizontal_scroll() {
        // 长行超过视口宽度。由于 textarea 未渲染，viewport top_col 初始为 0，
        // cursor_display_col=50, top_col=0, visible_col=min(50, 10-1)=9
        let mut ta = TextArea::default();
        ta.insert_str("a".repeat(50).as_str());
        let pos = textarea_cursor_pos(&ta, Rect::new(0, 0, 10, 1));
        assert_eq!(pos, Some((9, 0)));
    }

    #[test]
    fn test_cursor_pos_horizontal_scroll_with_offset() {
        // 验证当 tui-textarea 渲染后有真实 top_col 时的行为
        // 未渲染时 top_col=0，visible_col=min(100, 80-1)=79
        let mut ta = TextArea::default();
        ta.insert_str("a".repeat(100).as_str());
        let pos = textarea_cursor_pos(&ta, Rect::new(0, 0, 80, 1));
        assert_eq!(pos, Some((79, 0)));
    }

    #[test]
    fn test_cursor_pos_single_line_viewport() {
        // height=1：visible_height-1=0，scroll_row=cursor_row，visible_row=0
        let mut ta = TextArea::default();
        for _ in 0..5 {
            ta.insert_str("line\n");
        }
        let pos = textarea_cursor_pos(&ta, Rect::new(0, 0, 80, 1));
        assert_eq!(pos, Some((0, 0)));
    }

    #[test]
    fn test_cursor_pos_with_tab() {
        // tab 按 tab stop 对齐：'a' 占列 1，'\t' 跳到列 4（tab_len=4，pad=4-1=3）
        // 光标在 "a\t" 后的列索引 2，display column = 4
        let mut ta = TextArea::default();
        ta.insert_str("a\tb");
        // 光标默认在末尾 (0, 3)
        // display col before cursor: 'a'(1) + '\t'(pad 3) + 'b'(1) = 5
        let pos = textarea_cursor_pos(&ta, Rect::new(0, 0, 80, 24));
        assert_eq!(pos, Some((5, 0)));

        // 光标移动到 tab 之后但 'b' 之前：cursor_col=2，display col=4
        ta.move_cursor(tui_textarea::CursorMove::Jump(0, 2));
        let pos2 = textarea_cursor_pos(&ta, Rect::new(0, 0, 80, 24));
        assert_eq!(pos2, Some((4, 0)));
    }

    #[test]
    fn test_cursor_pos_non_zero_offset_with_scroll() {
        // 同时验证 (x, y) 非零起点 + 垂直滚动
        let mut ta = TextArea::default();
        for _ in 0..40 {
            ta.insert_str("x\n");
        }
        // cursor_row=40, visible_height=5: scroll_row=40-4=36, visible_row=40-36=4
        let pos = textarea_cursor_pos(&ta, Rect::new(10, 20, 80, 5));
        assert_eq!(pos, Some((10, 24)));
    }
}
