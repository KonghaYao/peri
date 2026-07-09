//! "Thought for N chars" 推理块组件。
//!
//! 显示推理过程的缩略预览：字符数 + 尾部最后 3 行。

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

use crate::i18n;
use crate::kit::theme;
use fluent_bundle::FluentValue;

/// 推理块属性。
#[derive(Props, Default)]
pub struct ReasoningBlockProps {
    /// 推理文本内容。
    pub text: Arc<str>,
    /// 是否处于折叠状态。
    pub collapsed: bool,
}

#[component]
pub fn ReasoningBlock(props: &ReasoningBlockProps) -> impl Into<AnyElement<'static>> {
    let semantic = theme::semantic();
    let char_count = props.text.chars().count();

    let mut lines = vec![Line::from("")];
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "render-thought-for",
            &[("count".to_string(), FluentValue::from(char_count as u64))],
        ),
        Style::default()
            .fg(semantic.text.dim)
            .add_modifier(Modifier::ITALIC),
    )]));

    if !props.collapsed {
        let tail_lines: Vec<&str> = props.text.lines().rev().take(3).collect();
        for tail in tail_lines.into_iter().rev() {
            if !tail.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(" ⎿ ", Style::default().fg(semantic.text.dim)),
                    Span::styled(tail.to_string(), Style::default().fg(semantic.text.dim)),
                ]));
            }
        }
    }
    lines.push(Line::from(""));

    element! {
        View(width: Constraint::Fill(1)) {
            Text(text: Paragraph::new(lines))
        }
    }
}
