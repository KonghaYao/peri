//! 消息区域 Widget 桥接组件。
//!
//! Phase 5 编译桩：将现有 `message_area.rs` 渲染逻辑桥接为 ratatui `Widget` trait。
//! 当前为占位实现，后续 Phase 将接入实际渲染逻辑。

use ratatui::{buffer::Buffer, layout::Rect, style::Style, text::Line, widgets::Widget};

use crate::ui::theme;

pub struct MessageAreaWidget;

impl Widget for &MessageAreaWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let line = Line::from("Message Area (Phase 5 bridge)").centered();
        ratatui::widgets::Paragraph::new(line)
            .style(Style::new().fg(theme::TEXT))
            .render(area, buf);
    }
}
