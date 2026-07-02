//! Peri branded welcome / landing 组件。
//!
//! 仅用于空消息态，占位聊天区内容；不承载业务逻辑。

#![allow(clippy::needless_update)]

use crate::kit::theme;
use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Flex},
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

const LOGO: &[&str] = &[
    "██████╗ ███████╗██████╗ ██╗",
    "██╔══██╗██╔════╝██╔══██╗██║",
    "██████╔╝█████╗  ██████╔╝██║",
    "██╔═══╝ ██╔══╝  ██╔══██╗██║",
    "██║     ███████╗██║  ██║██║",
    "╚═╝     ╚══════╝╚═╝  ╚═╝╚═╝",
];

const NARROW_THRESHOLD: usize = 50;

#[derive(Default, Props)]
pub struct WelcomeProps {
    pub width: usize,
}

#[component]
pub fn Welcome(props: &WelcomeProps) -> impl Into<AnyElement<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let narrow = props.width < NARROW_THRESHOLD;

    if narrow {
        lines.push(Line::from(Span::styled(
            "Peri",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(""));
        for row in LOGO {
            lines.push(Line::from(Span::styled(
                row.to_string(),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Your AI operating system for code, tools, and workflows",
        Style::default().fg(theme::MUTED),
    )));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "────────────────────────────────────────",
        Style::default().fg(theme::DIM),
    )));

    lines.push(Line::from(""));
    for feature in [
        "Code across the repo with shared context",
        "Open files, run tools, and inspect results",
        "Delegate work to agents and workflows",
    ] {
        lines.push(Line::from(vec![
            Span::styled(" • ", Style::default().fg(theme::ACCENT)),
            Span::styled(feature, Style::default().fg(theme::TEXT)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" /model", Style::default().fg(theme::WARNING)),
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled("/agents", Style::default().fg(theme::WARNING)),
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled("/tasks", Style::default().fg(theme::WARNING)),
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled("/help", Style::default().fg(theme::WARNING)),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" Enter", Style::default().fg(theme::DIM)),
        Span::styled(" send", Style::default().fg(theme::DIM)),
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled("Shift+Enter", Style::default().fg(theme::DIM)),
        Span::styled(" newline", Style::default().fg(theme::DIM)),
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled("@", Style::default().fg(theme::DIM)),
        Span::styled(" mention files", Style::default().fg(theme::DIM)),
    ]));

    let centered_lines: Vec<Line<'static>> =
        lines.into_iter().map(|line| line.centered()).collect();

    element!(
        View(
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
            justify_content: Flex::Center,
        ) {
            Text(text: Paragraph::new(centered_lines))
        }
    )
}
