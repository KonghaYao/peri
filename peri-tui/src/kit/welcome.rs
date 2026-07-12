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

// "peri" 四个字母的位图定义，每行用 '#' 表示实心、空格表示空白。
// 每行渲染为空白字符 Span：'#' → bg=active 的空格，' ' → 默认背景的空格。
const LETTER_P: &[&str] = &["######", "#    #", "######", "#     ", "#     "];
const LETTER_E: &[&str] = &["######", "#     ", "##### ", "#     ", "######"];
const LETTER_R: &[&str] = &["######", "#    #", "######", "#   # ", "#    #"];
const LETTER_I: &[&str] = &["######", "  ##  ", "  ##  ", "  ##  ", "######"];
const LETTERS: &[&[&str]] = &[LETTER_P, LETTER_E, LETTER_R, LETTER_I];

/// 将一个字母位图行转换为 Span 序列：'#' → 有色背景空格，' ' → 无背景空格。
/// 相邻同类像素会合并为一个 Span。
fn letter_row_to_spans(row: &str, color: Color) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current_filled: Option<bool> = None;
    let mut run_len: usize = 0;

    for ch in row.chars() {
        let filled = ch == '#';
        if current_filled == Some(filled) {
            run_len += 1;
        } else {
            if let Some(was_filled) = current_filled {
                let style = if was_filled {
                    Style::default().bg(color)
                } else {
                    Style::default()
                };
                spans.push(Span::styled(" ".repeat(run_len), style));
            }
            current_filled = Some(filled);
            run_len = 1;
        }
    }
    // flush last run
    if let Some(filled) = current_filled {
        let style = if filled {
            Style::default().bg(color)
        } else {
            Style::default()
        };
        spans.push(Span::styled(" ".repeat(run_len), style));
    }
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
        // 逐行渲染四个字母，空白字符反色形成字母轮廓
        for row_idx in 0..LETTER_P.len() {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (li, letter) in LETTERS.iter().enumerate() {
                if li > 0 {
                    // 字母间一个空格的间隙
                    spans.push(Span::styled(" ", Style::default()));
                }
                spans.extend(letter_row_to_spans(letter[row_idx], active_color));
            }
            lines.push(Line::from(spans));
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
