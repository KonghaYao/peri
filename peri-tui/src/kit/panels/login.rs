//! ratatui-kit LoginPanel component.
//!
//! Phase 6d 最后一批: provider 管理面板，支持 4 种模式（Browse/Edit/New/ConfirmDelete）。
//! 使用 use_state 管理 cursor + edit_buffer + mode，use_local_events 处理键盘事件。
//! Mock 数据 4 个 provider；Phase 8 通过 Atom/props 注入真实 provider 配置。
//!
//! 旧版: panel/panels/login.rs (PanelState trait).

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
// Mock provider data
// ---------------------------------------------------------------------------

/// Mock provider entry (Phase 8: from real config).
#[allow(dead_code)]
struct ProviderEntry {
    name: &'static str,
    api_key: &'static str,
    provider_type: &'static str,
}

/// Return the display name of a provider (Phase 8: tr("unnamed")).
#[allow(dead_code)]
fn display_name(name: &str) -> &str {
    if name.is_empty() { "Unnamed" } else { name }
}

/// Mask an API key for display: keep first 4 and last 4 chars.
#[allow(dead_code)]
fn mask_api_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let len = chars.len();
    if len <= 8 {
        return "*".repeat(len);
    }
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[len - 4..].iter().collect();
    format!("{}****{}", prefix, suffix)
}

#[allow(dead_code)]
const PROVIDERS: &[ProviderEntry] = &[
    ProviderEntry {
        name: "Anthropic",
        api_key: "sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
        provider_type: "anthropic",
    },
    ProviderEntry {
        name: "OpenAI",
        api_key: "sk-proj-zyxwvutsrqponmlkjihgfedcba",
        provider_type: "openai",
    },
    ProviderEntry {
        name: "OpenRouter",
        api_key: "sk-or-v1-1234567890abcdef1234567890abcdef",
        provider_type: "openrouter",
    },
    ProviderEntry {
        name: "Google",
        api_key: "AIzaSyD1234567890abcdefghijklmnopqr",
        provider_type: "google",
    },
];

// ---------------------------------------------------------------------------
// Mode enum
// ---------------------------------------------------------------------------

/// Panel mode.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
enum Mode {
    Browse,
    Edit,
    New,
    ConfirmDelete,
}

// ---------------------------------------------------------------------------
// LoginPanel component
// ---------------------------------------------------------------------------

#[component]
fn LoginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);
    let mode = hooks.use_state(|| Mode::Browse);
    let edit_buffer = hooks.use_state(String::new);

    let count = PROVIDERS.len();

    hooks.use_local_events({
        let cursor = cursor.clone();
        let mode = mode.clone();
        let edit_buffer = edit_buffer.clone();
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                let current_mode = *mode.read();
                match current_mode {
                    // -- Common: Esc/q always closes (or returns to Browse) --
                    Mode::Browse => match key.code {
                        KeyCode::Esc | KeyCode::Char('q') => {
                            // Phase 8: close panel via use_input_layer
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let mut c = cursor.write();
                            *c = c.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let mut c = cursor.write();
                            if count > 0 {
                                *c = (*c + 1).min(count - 1);
                            }
                        }
                        KeyCode::Enter => {
                            // Enter Edit mode on selected provider
                            let sel = *cursor.read();
                            if sel < PROVIDERS.len() {
                                let key = PROVIDERS[sel].api_key.to_string();
                                *edit_buffer.write() = key;
                                *mode.write() = Mode::Edit;
                            }
                        }
                        KeyCode::Char('n') => {
                            // Enter New mode
                            edit_buffer.write().clear();
                            *mode.write() = Mode::New;
                        }
                        KeyCode::Char('d') => {
                            // Enter ConfirmDelete mode
                            if count > 0 {
                                *mode.write() = Mode::ConfirmDelete;
                            }
                        }
                        _ => {}
                    },

                    Mode::Edit | Mode::New => match key.code {
                        KeyCode::Esc => {
                            // Cancel editing, back to Browse
                            *mode.write() = Mode::Browse;
                        }
                        KeyCode::Char('q') => {
                            *mode.write() = Mode::Browse;
                        }
                        KeyCode::Enter => {
                            // Save and return to Browse
                            // Phase 8: emit config update
                            *mode.write() = Mode::Browse;
                        }
                        KeyCode::Backspace => {
                            let mut buf = edit_buffer.write();
                            buf.pop();
                        }
                        KeyCode::Char(c) => {
                            edit_buffer.write().push(c);
                        }
                        _ => {}
                    },

                    Mode::ConfirmDelete => match key.code {
                        KeyCode::Char('y') | KeyCode::Enter => {
                            // Confirm delete
                            // Phase 8: emit delete_provider
                            *mode.write() = Mode::Browse;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            // Cancel delete
                            *mode.write() = Mode::Browse;
                        }
                        _ => {
                            // Any other key cancels
                            *mode.write() = Mode::Browse;
                        }
                    },
                }
            }
        }
    });

    let sel = *cursor.read();
    let current_mode = *mode.read();
    let edit_buf = edit_buffer.read();

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Empty line at top
    lines.push(Line::from(""));

    match current_mode {
        Mode::Browse => {
            // Provider list
            for (i, p) in PROVIDERS.iter().enumerate() {
                let is_cursor = i == sel;
                let bullet = if is_cursor { "\u{25cf}" } else { "\u{25cb}" };
                let cursor_mark = if is_cursor { "\u{276f}" } else { " " };

                let row_style = if is_cursor {
                    Style::new().fg(theme::THINKING)
                } else {
                    Style::new().fg(theme::TEXT)
                };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {} ", cursor_mark),
                        Style::new().fg(theme::THINKING),
                    ),
                    Span::styled(format!("{} ", bullet), row_style),
                    Span::styled(format!("{} ", display_name(p.name)), row_style.bold()),
                    Span::styled(
                        format!("({})", p.provider_type),
                        Style::new().fg(theme::MUTED),
                    ),
                ]));

                // Sub-row: API key
                let masked = mask_api_key(p.api_key);
                lines.push(Line::from(vec![
                    Span::styled("       api_key: ", Style::new().fg(theme::MUTED).bold()),
                    Span::styled(masked, Style::new().fg(theme::MUTED)),
                ]));
            }

            if PROVIDERS.is_empty() {
                lines.push(Line::from(""));
                lines.push(
                    Line::from("  No providers configured. Press 'n' to add one.").fg(theme::MUTED),
                );
            }

            // Footer hints
            lines.push(Line::from(""));
            lines.push(
                Line::from("  e/Enter) Edit  n) New  d) Delete  q/Esc) Close").fg(theme::DIM),
            );
        }

        Mode::Edit => {
            // Show selected provider info
            if let Some(p) = PROVIDERS.get(sel) {
                lines.push(Line::from(vec![
                    Span::styled("  Editing: ", Style::new().fg(theme::MUTED)),
                    Span::styled(
                        display_name(p.name),
                        Style::new().fg(theme::THINKING).bold(),
                    ),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("  Type: ", Style::new().fg(theme::MUTED)),
                    Span::styled(p.provider_type, Style::new().fg(theme::TEXT)),
                ]));
            }
            lines.push(Line::from(""));

            // API Key edit field
            let masked = mask_api_key(&edit_buf);
            lines.push(Line::from(vec![
                Span::styled("  API Key: ", Style::new().fg(theme::THINKING).bold()),
                Span::styled(format!("{}|", masked), Style::new().fg(theme::TEXT)),
                Span::styled(" (editing)", Style::new().fg(theme::MUTED)),
            ]));

            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "  Enter API key, press Enter to save",
                Style::new().fg(theme::DIM),
            )]));

            // Footer hints
            lines.push(Line::from(""));
            lines.push(Line::from("  Enter) Save  Esc/q) Cancel").fg(theme::DIM));
        }

        Mode::New => {
            lines.push(Line::from(vec![Span::styled(
                "  New Provider",
                Style::new().fg(theme::THINKING).bold(),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "  Type: openai (default)",
                Style::new().fg(theme::MUTED),
            )]));
            lines.push(Line::from(""));

            // API Key edit field
            let masked = if edit_buf.is_empty() {
                String::new()
            } else {
                mask_api_key(&edit_buf)
            };
            lines.push(Line::from(vec![
                Span::styled("  API Key: ", Style::new().fg(theme::THINKING).bold()),
                Span::styled(format!("{}|", masked), Style::new().fg(theme::TEXT)),
                Span::styled(" (editing)", Style::new().fg(theme::MUTED)),
            ]));

            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "  Enter API key, press Enter to save",
                Style::new().fg(theme::DIM),
            )]));

            // Footer hints
            lines.push(Line::from(""));
            lines.push(Line::from("  Enter) Save  Esc/q) Cancel").fg(theme::DIM));
        }

        Mode::ConfirmDelete => {
            // Provider list for context
            for (i, p) in PROVIDERS.iter().enumerate() {
                let is_cursor = i == sel;
                let bullet = if is_cursor { "\u{25cf}" } else { "\u{25cb}" };
                let cursor_mark = if is_cursor { "\u{276f}" } else { " " };
                let row_style = if is_cursor {
                    Style::new().fg(theme::THINKING)
                } else {
                    Style::new().fg(theme::TEXT)
                };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {} ", cursor_mark),
                        Style::new().fg(theme::THINKING),
                    ),
                    Span::styled(format!("{} ", bullet), row_style),
                    Span::styled(display_name(p.name).to_string(), row_style.bold()),
                ]));
            }

            // Confirmation line at bottom
            if let Some(p) = PROVIDERS.get(sel) {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("  Delete ", Style::new().fg(theme::TEXT)),
                    Span::styled(display_name(p.name), Style::new().fg(theme::ERROR).bold()),
                    Span::styled("? (y/n)", Style::new().fg(theme::TEXT)),
                ]));
            }

            // Footer hints
            lines.push(Line::from(""));
            lines.push(Line::from("  y) Confirm  n/Esc) Cancel").fg(theme::DIM));
        }
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Login ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(56),
            height: Constraint::Length(18),
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
