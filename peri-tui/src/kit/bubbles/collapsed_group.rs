//! 折叠/展开组组件。
//!
//! 显示 "● title（N 项）" 折叠摘要。

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::Style,
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::kit::theme;

/// 折叠组属性。
#[derive(Props, Default)]
pub struct CollapsedGroupProps {
    /// 组标题。
    pub title: String,
    /// 组内项数。
    pub count: u32,
}

#[component]
pub fn CollapsedGroup(props: &CollapsedGroupProps) -> impl Into<AnyElement<'static>> {
    let semantic = theme::semantic();
    let lines = vec![Line::from(vec![
        Span::styled("● ", Style::default().fg(semantic.status.success)),
        Span::styled(
            format!("{}（{} 项）", props.title, props.count),
            Style::default().fg(semantic.text.muted),
        ),
    ])];

    element! {
        View(width: Constraint::Fill(1)) {
            Text(text: Paragraph::new(lines))
        }
    }
}
