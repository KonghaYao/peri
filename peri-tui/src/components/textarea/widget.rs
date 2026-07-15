use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::{StatefulWidget, StatefulWidgetRef},
};

use super::render::render_multiline_with_cursor;
use super::state::TextAreaState;

/// 多行文本编辑区域 ratatui widget。
///
/// 渲染时将 TextAreaState 中的文本按 `\n` 拆分为多行，
/// 光标以指定 style 高亮。支持 loading 态（空白行）、选区高亮和占位符。
pub struct TextArea {
    cursor_style: Style,
    selection_style: Style,
    placeholder_style: Style,
    loading: bool,
    show_cursor: bool,
}

impl TextArea {
    pub fn new(cursor_style: Style, selection_style: Style) -> Self {
        Self {
            cursor_style,
            selection_style,
            placeholder_style: Style::default(),
            loading: false,
            show_cursor: true,
        }
    }

    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn show_cursor(mut self, show_cursor: bool) -> Self {
        self.show_cursor = show_cursor;
        self
    }

    pub fn placeholder_style(mut self, style: Style) -> Self {
        self.placeholder_style = style;
        self
    }
}

impl StatefulWidgetRef for TextArea {
    type State = TextAreaState;

    fn render_ref(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let placeholder = if state.placeholder.is_empty() {
            None
        } else {
            Some(state.placeholder.as_str())
        };
        let max_width = area.width.saturating_sub(2).max(1) as usize;
        let viewport_height = area.height as usize;
        let default_style = Style::default().bg(Color::Black);
        let lines = render_multiline_with_cursor(
            &state.text,
            state.cursor,
            self.cursor_style,
            state.selection_range(),
            self.selection_style,
            placeholder,
            self.placeholder_style,
            default_style,
            max_width,
            viewport_height,
            self.loading,
            self.show_cursor,
        );
        for (i, line) in lines.into_iter().enumerate() {
            if i as u16 >= area.height {
                break;
            }
            let row = area.y + i as u16;
            buf.set_line(area.x, row, &line, area.width);
        }
    }
}

impl StatefulWidget for TextArea {
    type State = TextAreaState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        self.render_ref(area, buf, state);
    }
}
