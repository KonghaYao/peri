//! ratatui-kit HooksPanel component.
//!
//! Phase 6a: read-only hook list with cursor navigation (use_state +
//! use_local_events). Mock data; Phase 8 通过 Atom/props 注入真实 hook 列表。
//!
//! 旧版: panel/panels/hooks.rs (PanelState trait, HookDto-based).
//! Hooks are configured via plugin hooks/hooks.json files; this panel is
//! display-only.

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::ui::theme;

/// Mock hook entry (Phase 8: injected via Atom from HookDto).
#[allow(dead_code)]
struct HookEntry {
    event: &'static str,
    command: &'static str,
    enabled: bool,
}

#[allow(dead_code)]
const HOOK_ENTRIES: &[HookEntry] = &[
    HookEntry {
        event: "PreToolUse",
        command: "echo 'About to use tool' | tee -a /tmp/hooks.log",
        enabled: true,
    },
    HookEntry {
        event: "PostToolUse",
        command: "echo 'Tool used' | tee -a /tmp/hooks.log",
        enabled: true,
    },
    HookEntry {
        event: "PostToolUseFailure",
        command: "echo 'Tool failed' | tee -a /tmp/hooks.log",
        enabled: false,
    },
    HookEntry {
        event: "Notification",
        command: "osascript -e 'display notification \"Agent needs input\"'",
        enabled: true,
    },
    HookEntry {
        event: "SessionStart",
        command: "echo 'Session started at' $(date)",
        enabled: true,
    },
    HookEntry {
        event: "SessionEnd",
        command: "echo 'Session ended at' $(date)",
        enabled: false,
    },
    HookEntry {
        event: "Stop",
        command: "echo 'Agent stopped'",
        enabled: true,
    },
    HookEntry {
        event: "PreCompact",
        command: "echo 'About to compact context'",
        enabled: false,
    },
];

/// Map event name to human-readable description.
fn event_description(event: &str) -> &'static str {
    match event {
        "PreToolUse" => "Before tool execution",
        "PostToolUse" => "After tool execution",
        "PostToolUseFailure" => "After tool execution fails",
        "PermissionRequest" => "Before auto mode classifier decides",
        "UserPromptSubmit" => "When user submits a prompt",
        "SessionStart" => "When a new session starts",
        "SessionEnd" => "When a session ends",
        "Stop" => "When agent stops",
        "StopFailure" => "When agent stops with failure",
        "PostToolBatch" => "When all parallel tools complete",
        "SubagentStart" => "When a subagent starts",
        "SubagentStop" => "When a subagent stops",
        "PreCompact" => "Before context compaction",
        "PostCompact" => "After context compaction",
        "Notification" => "When agent needs user input",
        _ => "",
    }
}

#[component]
fn HooksPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);

    hooks.use_local_events({
        let selected = selected;
        let count = HOOK_ENTRIES.len();
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
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Ctrl+C: don't consume, let upper layers handle
                        return;
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // Stats line
    if !HOOK_ENTRIES.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("  {} hooks configured", HOOK_ENTRIES.len()),
            Style::new().fg(theme::TEXT).bold(),
        )]));
    }

    // Read-only hint
    lines.push(Line::from(vec![Span::styled(
        "  (read-only — configured via plugins)",
        Style::new().fg(theme::MUTED),
    )]));
    lines.push(Line::from(""));

    // Hook list
    if HOOK_ENTRIES.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No hooks configured",
            Style::new().fg(theme::MUTED),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Add hooks/<event>.json files to a plugin",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in HOOK_ENTRIES.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };
            let enabled_label = if entry.enabled { "ON" } else { "OFF" };
            let enabled_style = if entry.enabled {
                Style::new().fg(theme::SAGE)
            } else {
                Style::new().fg(theme::MUTED)
            };
            let desc = event_description(entry.event);

            // Label line: cursor + num + event + [ON/OFF] + description
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {}. {} ", cursor, i + 1, entry.event),
                    name_style,
                ),
                Span::styled(format!("[{}]", enabled_label), enabled_style),
                Span::styled(format!("  {}", desc), Style::new().fg(theme::MUTED)),
            ]));

            // Detail line: command summary (indented, truncated)
            let cmd_summary: String = entry
                .command
                .chars()
                .take(50)
                .chain(if entry.command.chars().count() > 50 {
                    Some('…')
                } else {
                    None
                })
                .collect();
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", cmd_summary),
                Style::new().fg(theme::TEXT),
            )]));
        }
    }

    // Footer hints
    lines.push(Line::from(""));
    lines.push(Line::from("  ↑↓) Navigate  Esc) Close").fg(theme::DIM));

    let content = if lines.is_empty() {
        Paragraph::new(Line::from("  (empty)").fg(theme::MUTED))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Hooks ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(56),
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
