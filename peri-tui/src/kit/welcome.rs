//! Peri branded welcome / landing 组件。
//!
//! 仅用于空消息态，占位聊天区内容；不承载业务逻辑。

#![allow(clippy::needless_update)]

use crate::i18n;
use crate::kit::atoms::LANG_VERSION;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Flex},
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
pub fn Welcome(props: &WelcomeProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang_ver = hooks.use_atom(&LANG_VERSION);
    let semantic = THEME_ATOM.state().read().semantic;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let narrow = props.width < NARROW_THRESHOLD;

    if narrow {
        lines.push(Line::from(Span::styled(
            "Peri",
            Style::default()
                .fg(semantic.border.active)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(""));
        for row in LOGO {
            lines.push(Line::from(Span::styled(
                row.to_string(),
                Style::default()
                    .fg(semantic.border.active)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Your AI operating system for code, tools, and workflows",
        Style::default().fg(semantic.text.muted),
    )));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "────────────────────────────────────────",
        Style::default().fg(semantic.text.dim),
    )));

    lines.push(Line::from(""));
    for feature in [
        i18n::tr("welcome-feature-code"),
        i18n::tr("welcome-feature-files"),
        i18n::tr("welcome-feature-agents"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(" • ", Style::default().fg(semantic.border.active)),
            Span::styled(feature, Style::default().fg(semantic.text.primary)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" /model", Style::default().fg(semantic.status.warning)),
        Span::styled("  ", Style::default().fg(semantic.text.dim)),
        Span::styled("/agents", Style::default().fg(semantic.status.warning)),
        Span::styled("  ", Style::default().fg(semantic.text.dim)),
        Span::styled("/tasks", Style::default().fg(semantic.status.warning)),
        Span::styled("  ", Style::default().fg(semantic.text.dim)),
        Span::styled("/help", Style::default().fg(semantic.status.warning)),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Enter", Style::default().fg(semantic.text.dim)),
        Span::styled(" send", Style::default().fg(semantic.text.dim)),
        Span::styled("  ", Style::default().fg(semantic.text.dim)),
        Span::styled("Shift+Enter", Style::default().fg(semantic.text.dim)),
        Span::styled(" newline", Style::default().fg(semantic.text.dim)),
        Span::styled("  ", Style::default().fg(semantic.text.dim)),
        Span::styled("@", Style::default().fg(semantic.text.dim)),
        Span::styled(" mention files", Style::default().fg(semantic.text.dim)),
    ]));

    let centered_lines: Vec<Line<'static>> =
        lines.into_iter().map(|line| line.centered()).collect();
    let welcome_height = centered_lines.len() as u16;

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
            justify_content: Flex::Center,
        ) {
            View(width: Constraint::Fill(1), height: Constraint::Length(welcome_height)) {
                Text(text: Paragraph::new(centered_lines))
            }
        }
    )
}
