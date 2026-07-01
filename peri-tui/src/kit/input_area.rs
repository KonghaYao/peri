//! 输入区域 Widget 桥接组件。
//!
//! Phase 5 编译桩：将 `tui-textarea-2` 桥接为 ratatui `StatefulWidget` trait。
//! 当前为占位实现，后续 Phase 将接入实际 textarea 渲染逻辑。

use ratatui::{
    buffer::Buffer, layout::Rect, style::Style, text::Line, widgets::StatefulWidget,
};

use crate::ui::theme;

/// 输入框外部状态。
#[derive(Clone, Default)]
pub struct TextareaState {
    pub focused: bool,
    pub loading: bool,
}

/// 输入区域 Widget——桥接 tui_textarea 为 StatefulWidget。
pub struct TextareaWidget<'a> {
    pub textarea: &'a mut tui_textarea::TextArea<'static>,
    pub state: &'a TextareaState,
}

impl StatefulWidget for TextareaWidget<'_> {
    type State = TextareaState;
    fn render(self, area: Rect, buf: &mut Buffer, _state: &mut Self::State) {
        let line = Line::from("Input Area (Phase 5 bridge)").centered();
        ratatui::widgets::Paragraph::new(line)
            .style(Style::new().fg(theme::TEXT))
            .render(area, buf);
    }
}
