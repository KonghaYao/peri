//! ratatui-kit MemoryPanel component.
//!
//! Phase 6b: memory entry list with cursor navigation (use_state + use_local_events).
//! Mock data; Phase 8 通过 Atom/props 注入真实 memory 条目。
//!
//! 旧版: panel/panels/memory.rs (PanelState trait, MemoryEntry-based).

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
use crate::ui::theme;

/// Mock memory entry (Phase 8: injected via Atom from real memory files).
#[allow(dead_code)]
struct MemoryEntry {
    label: &'static str,
    path: &'static str,
    exists: bool,
}

#[allow(dead_code)]
const MEMORY_ENTRIES: &[MemoryEntry] = &[
    MemoryEntry {
        label: "Project",
        path: "CLAUDE.md",
        exists: true,
    },
    MemoryEntry {
        label: "User",
        path: "~/.claude/CLAUDE.md",
        exists: true,
    },
    MemoryEntry {
        label: "Skills",
        path: ".claude/skills/*/SKILL.md",
        exists: true,
    },
    MemoryEntry {
        label: "Rules",
        path: ".claude/rules/",
        exists: false,
    },
];

#[component]
fn MemoryPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let selected = selected.clone();
        let count = MEMORY_ENTRIES.len();
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
                        let mut s = selected.write();
                        *s = s.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let mut s = selected.write();
                        if count > 0 {
                            *s = (*s + 1).min(count - 1);
                        }
                    }
                    KeyCode::Enter => {
                        // TODO Phase 8: open editor for selected entry
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Memory list
    if MEMORY_ENTRIES.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No memory files",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in MEMORY_ENTRIES.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let label_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };

            let (exist_icon, exist_style) = if entry.exists {
                ("Y", Style::new().fg(theme::SAGE))
            } else {
                ("N", Style::new().fg(theme::MUTED))
            };

            lines.push(Line::from(vec![
                Span::styled(cursor.to_string(), label_style),
                Span::styled(format!(" [{}] ", exist_icon), exist_style),
                Span::styled(
                    format!("{:<12} ", entry.label),
                    label_style,
                ),
                Span::styled(
                    entry.path,
                    Style::new().fg(theme::MUTED),
                ),
            ]));
        }
    }

    // Bottom hint line
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  j/k) Navigate  Enter) Edit  q) Close",
        Style::new().fg(theme::MUTED),
    )]));

    let content = if lines.is_empty() {
        Paragraph::new(Line::from("  (empty)").fg(theme::MUTED))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Memory ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(50),
            height: Constraint::Length(14),
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
