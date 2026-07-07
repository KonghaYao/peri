use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    widgets::{StatefulWidget, StatefulWidgetRef},
};

use super::render::render_multiline_with_cursor;
use super::state::TextAreaState;

/// 多行文本编辑区域 ratatui widget。
///
/// 渲染时将 TextAreaState 中的文本按 `\n` 拆分为多行，
/// 光标以指定 style 高亮。支持 loading 态（空白行）和选区高亮。
pub struct TextArea {
    cursor_style: Style,
    selection_style: Style,
    loading: bool,
}

impl TextArea {
    pub fn new(cursor_style: Style, selection_style: Style) -> Self {
        Self {
            cursor_style,
            selection_style,
            loading: false,
        }
    }

    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }
}

impl StatefulWidgetRef for TextArea {
    type State = TextAreaState;

    fn render_ref(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let lines = render_multiline_with_cursor(
            &state.text,
            state.cursor,
            self.cursor_style,
            state.selection_range(),
            self.selection_style,
            self.loading,
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
