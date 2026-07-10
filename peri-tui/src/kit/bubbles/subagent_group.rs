//! 子 Agent 消息递归容器。
//!
//! 显示 agent 名称头行 + 缩进子内容 + 最终结果摘要。
//! 子 ViewModel 由 MessageArea 父组件通过 dispatch 函数预渲染后传入 children。

use std::sync::Arc;

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use peri_theme::atoms::THEME_ATOM;

/// 子 Agent 消息组属性。
#[derive(Props, Default)]
pub struct SubAgentGroupProps {
    /// SubAgent 名称。
    pub agent_name: String,
    /// 是否正在运行。
    pub is_running: bool,
    /// 是否以错误结束。
    pub is_error: bool,
    /// 是否折叠。
    pub collapsed: bool,
    /// 子内容行（已应用缩进）。由 MessageArea 预计算传入。
    pub body_lines: Vec<Line<'static>>,
    /// 最终结果文本（如果完成且有结果）。
    pub final_result: Option<Arc<str>>,
}

#[component]
pub fn SubAgentGroup(
    mut hooks: Hooks,
    props: &SubAgentGroupProps,
) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // ── Header 行 ─────────────────────────────────────────────────────────
    let arrow = if props.collapsed { "▶" } else { "▼" };
    let prefix = Span::styled(
        format!("{} ◆ ", arrow),
        Style::default()
            .fg(semantic.text.dim)
            .add_modifier(Modifier::BOLD),
    );
    let name = Span::styled(
        props.agent_name.clone(),
        Style::default().fg(semantic.text.primary),
    );
    let mut header_spans = vec![prefix, name];

    if props.is_running {
        header_spans.push(Span::styled(
            " …",
            Style::default()
                .fg(semantic.status.warning)
                .add_modifier(Modifier::ITALIC),
        ));
    } else if props.is_error {
        header_spans.push(Span::styled(
            " ✗",
            Style::default().fg(semantic.status.error),
        ));
    }
    lines.push(Line::from(header_spans));

    // ── Body（缩进子内容） ────────────────────────────────────────────────
    if !props.collapsed {
        lines.extend(props.body_lines.clone());
    }

    // ── Final Result ─────────────────────────────────────────────────────
    if let Some(ref result) = props.final_result {
        let color = if props.is_error {
            semantic.status.error
        } else {
            semantic.text.muted
        };
        let preview_lines: Vec<&str> = result
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(3)
            .collect();
        for line in preview_lines {
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                Span::styled(line.to_string(), Style::default().fg(color)),
            ]));
        }
        if result.lines().filter(|l| !l.trim().is_empty()).count() > 3 {
            lines.push(Line::from(vec![Span::styled(
                "  ...".to_string(),
                Style::default().fg(semantic.text.dim),
            )]));
        }
    }

    // 末尾空行
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }

    element! {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
        ) {
            Text(text: Paragraph::new(lines))
        }
    }
}
