//! 用户消息气泡——纯 Span 拼接，不解析 markdown。

use std::sync::Arc;

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use peri_theme::atoms::THEME_ATOM;

/// 用户消息气泡属性。
#[derive(Props, Default)]
pub struct UserBubbleProps {
    /// 用户输入的原始文本（不解析 markdown）。
    pub content: Arc<str>,
}

#[component]
pub fn UserBubble(mut hooks: Hooks, props: &UserBubbleProps) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let component = &guard.component;
    let user_bg = component.message.user_bg;
    let lines: Vec<Line<'static>> = if props.content.is_empty() {
        vec![]
    } else {
        props
            .content
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let prefix_text = if i == 0 { "❯ " } else { "  " };
                let prefix = Span::styled(
                    prefix_text,
                    Style::default()
                        .fg(semantic.accent)
                        .add_modifier(Modifier::BOLD)
                        .bg(user_bg),
                );
                let text = Span::styled(line.to_string(), Style::default().bg(user_bg));
                Line::from(vec![prefix, text])
            })
            .collect()
    };

    element! {
        View(width: Constraint::Fill(1)) {
            Text(text: Paragraph::new(lines))
        }
    }
}
