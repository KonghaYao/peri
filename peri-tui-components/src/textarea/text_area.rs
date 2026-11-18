use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::widgets::StatefulWidget;
use ratatui::widgets::StatefulWidgetRef;

use super::render::{TextAreaRenderOptions, TextAreaStyles, render_multiline_with_cursor};
use super::state::TextAreaState;

/// 多行文本编辑区域 ratatui widget，使用与文本行组合入口相同的渲染逻辑。
/// 光标、选区、占位符与基础样式由调用方提供；不读取全局主题或焦点。
#[derive(Debug, Clone, Copy)]
pub struct TextArea {
    styles: TextAreaStyles,
    width_reserve: u16,
    show_cursor: bool,
}

impl TextArea {
    #[must_use]
    pub fn new(cursor_style: Style, selection_style: Style) -> Self {
        Self {
            styles: TextAreaStyles {
                cursor: cursor_style,
                selection: selection_style,
                placeholder: Style::default(),
                base: Style::default().bg(Color::Black),
            },
            width_reserve: 2,
            show_cursor: true,
        }
    }

    pub fn show_cursor(mut self, show_cursor: bool) -> Self {
        self.show_cursor = show_cursor;
        self
    }

    pub fn placeholder_style(mut self, style: Style) -> Self {
        self.styles.placeholder = style;
        self
    }

    /// Override the fallback style for cursor-line text and explicit row padding.
    /// The default black background preserves the existing widget appearance.
    pub fn base_style(mut self, style: Style) -> Self {
        self.styles.base = style;
        self
    }

    /// Columns reserved from wrapping width, without shifting the text origin.
    /// Defaults to two; the effective wrapping width is always at least one cell.
    /// A visible cursor reserves at least one column for its trailing cell, even
    /// when zero is requested. Hidden cursors allow the full width with zero.
    pub fn width_reserve(mut self, columns: u16) -> Self {
        self.width_reserve = columns;
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
        let width_reserve = if self.show_cursor {
            self.width_reserve.max(1)
        } else {
            self.width_reserve
        };
        let lines = render_multiline_with_cursor(TextAreaRenderOptions {
            text: &state.text,
            cursor: state.cursor,
            selection_range: state.selection_range(),
            placeholder,
            styles: self.styles,
            max_width: area.width.saturating_sub(width_reserve).max(1) as usize,
            viewport_height: area.height as usize,
            show_cursor: self.show_cursor,
        });
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

#[cfg(test)]
#[path = "text_area_test.rs"]
mod tests;
