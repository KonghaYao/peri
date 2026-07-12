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
        style::{Color, Modifier, Style},
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

/// 判断是否为边界字符（非 █ 非空格的双线框字符）
fn is_border(c: char) -> bool {
    matches!(c, '╗' | '╔' | '╝' | '╚' | '║' | '═')
}

/// 将 LOGO 行转为 Span 序列：
/// - █ → 空白字符 + bg=color（反色填充）
/// - ╗╔╝╚║═ → 保留字形 + fg=color（边框可见）
/// - 空格 → 空白字符 + 默认样式
/// 相邻同 (内容, 样式) 的 span 会合并。
fn logo_row_to_spans(row: &str, color: Color) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    // 缓存上一个积累的 (ch_repr, style)，用于合并相邻同类 span
    let mut pending_ch: Option<char> = None;
    let mut pending_style: Option<Style> = None;
    let mut pending_count: usize = 0;

    let flush =
        |spans: &mut Vec<Span<'static>>, ch: Option<char>, style: Option<Style>, count: usize| {
            if count == 0 || ch.is_none() {
                return;
            }
            let ch = ch.unwrap();
            let s = if ch == ' ' {
                " ".repeat(count)
            } else {
                ch.to_string().repeat(count)
            };
            spans.push(Span::styled(s, style.unwrap_or_default()));
        };

    for ch in row.chars() {
        let (out_ch, style) = if ch == ' ' {
            (' ', Style::default())
        } else if ch == '█' {
            (' ', Style::default().bg(color))
        } else if is_border(ch) {
            (ch, Style::default().fg(color))
        } else {
            (ch, Style::default().fg(color))
        };

        if pending_ch == Some(out_ch) && pending_style == Some(style) {
            pending_count += 1;
        } else {
            flush(&mut spans, pending_ch, pending_style, pending_count);
            pending_ch = Some(out_ch);
            pending_style = Some(style);
            pending_count = 1;
        }
    }
    flush(&mut spans, pending_ch, pending_style, pending_count);

    spans
}

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

    let active_color = semantic.border.active;

    if narrow {
        lines.push(Line::from(Span::styled(
            "Peri",
            Style::default()
                .fg(active_color)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(""));
        for row in LOGO {
            lines.push(Line::from(logo_row_to_spans(row, active_color)));
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
