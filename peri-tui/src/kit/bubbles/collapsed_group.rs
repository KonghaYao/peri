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

use peri_theme::atoms::THEME_ATOM;

/// 折叠组属性。
#[derive(Props, Default)]
pub struct CollapsedGroupProps {
    /// 组标题。
    pub title: String,
    /// 组内项数。
    pub count: u32,
}

#[component]
pub fn CollapsedGroup(
    mut hooks: Hooks,
    props: &CollapsedGroupProps,
) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let guard = theme_def.read();
    let semantic = &guard.semantic;
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
