//! 系统提示组件——Info/Warning/Error 三级。
//!
//! 根据文本内容自动检测前缀（✻/⎿）和关键词（失败/error/warning），
//! 应用对应颜色。

use std::sync::Arc;

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

/// 系统提示属性。
#[derive(Props, Default)]
pub struct SystemNoteProps {
    /// 系统提示文本内容。
    pub content: Arc<str>,
}

#[component]
pub fn SystemNote(mut hooks: Hooks, props: &SystemNoteProps) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let lines: Vec<Line<'static>> = props
        .content
        .lines()
        .map(|line_text| {
            let mut spans: Vec<Span<'static>> = Vec::new();

            let (prefix_str, color) = if line_text.starts_with('\u{273B}') {
                // ✻ 元信息前缀 → dim 色
                ("✻ ", semantic.text.dim)
            } else if line_text.starts_with("⎿") {
                if line_text.starts_with("  ⎿") {
                    ("  ⎿ ", semantic.status.error)
                } else {
                    ("⎿ ", semantic.text.muted)
                }
            } else if line_text.contains('\u{274C}')
                || line_text.contains("失败")
                || line_text.contains("error")
            {
                ("", semantic.status.error)
            } else if line_text.contains("warning") || line_text.contains("warn") {
                ("", semantic.status.warning)
            } else {
                ("", semantic.text.muted)
            };

            let content_text = if prefix_str.contains('\u{273B}') {
                spans.push(Span::styled(
                    "✻ ".to_string(),
                    Style::default().fg(semantic.text.dim),
                ));
                line_text
                    .strip_prefix('\u{273B}')
                    .unwrap_or(line_text)
                    .trim_start()
            } else if prefix_str.starts_with("  ") {
                spans.push(Span::styled(
                    "  ⎿ ".to_string(),
                    Style::default().fg(semantic.text.dim),
                ));
                line_text
                    .strip_prefix("  ⎿")
                    .unwrap_or(line_text)
                    .trim_start()
            } else if prefix_str.contains("⎿") {
                spans.push(Span::styled(
                    "⎿ ".to_string(),
                    Style::default().fg(semantic.text.dim),
                ));
                line_text
                    .strip_prefix("⎿")
                    .unwrap_or(line_text)
                    .trim_start()
            } else {
                line_text
            };

            if !content_text.is_empty() {
                spans.push(Span::styled(
                    content_text.to_string(),
                    Style::default().fg(color),
                ));
            }

            Line::from(spans)
        })
        .collect();

    element! {
        View(width: Constraint::Fill(1)) {
            Text(text: Paragraph::new(lines))
        }
    }
}
