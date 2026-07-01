//! ratatui-kit ModelPanel component.
//!
//! Phase 6c batch 1: model alias selection with cursor/settings navigation
//! (use_state + use_local_events). Mock data with 3 alias tiers
//! (Opus/Sonnet/Haiku), each with effort/max_tokens/1M context config.
//! Phase 8 通过 Atom/props 注入真实模型配置。
//!
//! 旧版: panel/panels/model.rs (PanelState trait).

use crate::ui::theme;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

// ---------------------------------------------------------------------------
// Mock model data
// ---------------------------------------------------------------------------

/// Mock model alias entry (Phase 8: from real config).
#[allow(dead_code)]
struct ModelAliasEntry {
    name: &'static str,
    key: &'static str,
    effort: &'static str,
    max_tokens: u32,
    context_1m: bool,
    model_id: &'static str,
}

#[allow(dead_code)]
const MODEL_ALIASES: &[ModelAliasEntry] = &[
    ModelAliasEntry {
        name: "Opus",
        key: "opus",
        effort: "high",
        max_tokens: 32000,
        context_1m: false,
        model_id: "claude-opus-4-20250514",
    },
    ModelAliasEntry {
        name: "Sonnet",
        key: "sonnet",
        effort: "high",
        max_tokens: 64000,
        context_1m: false,
        model_id: "claude-sonnet-4-20250514",
    },
    ModelAliasEntry {
        name: "Haiku",
        key: "haiku",
        effort: "low",
        max_tokens: 8000,
        context_1m: false,
        model_id: "claude-3-5-haiku-20241022",
    },
];

#[component]
fn ModelPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);
    // selected_tab stores the index of the selected alias
    let selected_tab = hooks.use_state(|| 1usize); // default Sonnet

    hooks.use_local_events({
        let cursor = cursor.clone();
        let selected_tab = selected_tab.clone();
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // TODO Phase 8: close panel via use_input_layer
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        let mut c = cursor.write();
                        *c = c.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let mut c = cursor.write();
                        *c = (*c + 1).min(MODEL_ALIASES.len() - 1);
                    }
                    KeyCode::Enter => {
                        // Select the currently highlighted alias
                        let sel = *cursor.read();
                        *selected_tab.write() = sel;
                    }
                    KeyCode::Left => {
                        // Cycle selected model left
                        let mut s = selected_tab.write();
                        *s = s.saturating_sub(1);
                        *cursor.write() = *s;
                    }
                    KeyCode::Right => {
                        // Cycle selected model right
                        let mut s = selected_tab.write();
                        *s = (*s + 1).min(MODEL_ALIASES.len() - 1);
                        *cursor.write() = *s;
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *cursor.read();
    let active = *selected_tab.read();
    let active_entry = &MODEL_ALIASES[active];

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Header
    lines.push(Line::from(vec![Span::styled(
        "  Model Alias Selection",
        Style::new().fg(theme::TEXT).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  --------------------",
        Style::new().fg(theme::DIM),
    )]));
    lines.push(Line::from(""));

    // Alias rows
    for (i, entry) in MODEL_ALIASES.iter().enumerate() {
        let is_selected = i == active;
        let is_cursor = i == sel;
        let cursor_mark = if is_cursor { "\u{276f}" } else { " " };
        let check = if is_selected { "\u{2714}" } else { " " };

        let name_style = if is_selected {
            Style::new().fg(theme::SAGE).bold()
        } else if is_cursor {
            Style::new().fg(theme::THINKING).bold()
        } else {
            Style::new().fg(theme::TEXT)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme::THINKING),
            ),
            Span::styled(format!("{:<10}", entry.name), name_style),
            Span::styled(
                format!(" {}", check),
                if is_selected {
                    Style::new().fg(theme::SAGE)
                } else {
                    Style::new().fg(theme::MUTED)
                },
            ),
            Span::styled(
                format!("  {}", entry.model_id),
                Style::new().fg(theme::MUTED),
            ),
        ]));
    }

    lines.push(Line::from(""));

    // Current selection details
    lines.push(Line::from(vec![Span::styled(
        format!("  Active: {}", active_entry.name),
        Style::new().fg(theme::ACCENT).bold(),
    )]));
    lines.push(Line::from(vec![
        Span::styled("  Model ID: ", Style::new().fg(theme::MUTED)),
        Span::styled(active_entry.model_id, Style::new().fg(theme::TEXT)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Effort: ", Style::new().fg(theme::MUTED)),
        Span::styled(active_entry.effort, Style::new().fg(theme::WARNING).bold()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Max Tokens: ", Style::new().fg(theme::MUTED)),
        Span::styled(
            active_entry.max_tokens.to_string(),
            Style::new().fg(theme::TEXT),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  1M Context: ", Style::new().fg(theme::MUTED)),
        Span::styled(
            if active_entry.context_1m { "ON" } else { "OFF" },
            if active_entry.context_1m {
                Style::new().fg(theme::SAGE)
            } else {
                Style::new().fg(theme::MUTED)
            },
        ),
    ]));

    // Footer
    lines.push(Line::from(""));
    lines.push(Line::from("  j/k) Nav  Enter) Select  ←/→) Switch  q) Close").fg(theme::DIM));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Model ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(50),
            height: Constraint::Length(16),
        ) {
            ScrollView(
                scroll_bars: ScrollBars::default(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
        }
    )
}
