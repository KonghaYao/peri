//! ratatui-kit ConfigPanel component.
//!
//! Phase 6d: configuration panel with toggle/cycle/text row types.
//! Mock data; Phase 8 通过 Atom/props 注入真实配置并连接持久化。
//!
//! 旧版: panel/panels/config.rs (PanelState trait).

#![allow(dead_code)]

use crate::kit::theme;
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
// Row type
// ---------------------------------------------------------------------------

#[allow(dead_code)]
enum RowType {
    Toggle,
    Cycle(&'static [&'static str]),
    Text,
}

// ---------------------------------------------------------------------------
// Mock config rows (Phase 8: from real config)
// ---------------------------------------------------------------------------

const CONFIG_ROWS: &[(&str, RowType)] = &[
    ("YOLO Mode", RowType::Toggle),
    ("Auto Compact", RowType::Toggle),
    (
        "Permission Mode",
        RowType::Cycle(&["default", "accept-edit", "auto-mode", "bypass"]),
    ),
    ("Context Threshold", RowType::Text),
    ("Max Iterations", RowType::Text),
    ("Show Diff", RowType::Toggle),
    ("Model", RowType::Cycle(&["opus", "sonnet", "haiku"])),
    ("Provider", RowType::Cycle(&["anthropic", "openai"])),
];

/// Human-readable labels for model cycle options.
const MODEL_LABELS: &[&str] = &[
    "claude-opus-4-20250514",
    "claude-sonnet-4-20250514",
    "claude-3-5-haiku-20241022",
];

// ---------------------------------------------------------------------------
// Toggle row indices (used to map row index to state variable)
// ---------------------------------------------------------------------------

const ROW_YOLO: usize = 0;
const ROW_AUTO_COMPACT: usize = 1;
const ROW_PERMISSION: usize = 2;
const ROW_THRESHOLD: usize = 3;
const ROW_MAX_ITER: usize = 4;
const ROW_SHOW_DIFF: usize = 5;
const ROW_MODEL: usize = 6;
const ROW_PROVIDER: usize = 7;

#[component]
pub fn ConfigPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);
    // Toggle states (default: YOLO=OFF, AutoCompact=ON, ShowDiff=ON)
    let yolo = hooks.use_state(|| false);
    let auto_compact = hooks.use_state(|| true);
    let show_diff = hooks.use_state(|| true);
    // Cycle indices
    let perm_idx = hooks.use_state(|| 0usize);
    let model_idx = hooks.use_state(|| 1usize); // default sonnet
    let provider_idx = hooks.use_state(|| 0usize);
    // Text values
    let threshold = hooks.use_state(|| String::from("0.85"));
    let max_iter = hooks.use_state(|| String::from("500"));

    hooks.use_local_events({
        let cursor = cursor.clone();
        let yolo = yolo.clone();
        let auto_compact = auto_compact.clone();
        let show_diff = show_diff.clone();
        let perm_idx = perm_idx.clone();
        let model_idx = model_idx.clone();
        let provider_idx = provider_idx.clone();
        let threshold = threshold.clone();
        let max_iter = max_iter.clone();
        let row_count = CONFIG_ROWS.len();

        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                let sel = *cursor.read();

                match key.code {
                    // Close
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // TODO Phase 8: close panel via use_input_layer
                    }
                    // Navigate up
                    KeyCode::Up | KeyCode::Char('k') => {
                        *cursor.write() = sel.saturating_sub(1);
                    }
                    // Navigate down
                    KeyCode::Down | KeyCode::Char('j') => {
                        *cursor.write() = (sel + 1).min(row_count - 1);
                    }
                    // Space: toggle/cycle on current row
                    KeyCode::Char(' ') => {
                        let row_type = &CONFIG_ROWS[sel].1;
                        match row_type {
                            RowType::Toggle => toggle_row(sel, &yolo, &auto_compact, &show_diff),
                            RowType::Cycle(opts) => {
                                cycle_forward(sel, opts, &perm_idx, &model_idx, &provider_idx)
                            }
                            RowType::Text => {
                                // Insert space on text rows
                                match sel {
                                    ROW_THRESHOLD => threshold.write().push(' '),
                                    ROW_MAX_ITER => max_iter.write().push(' '),
                                    _ => {}
                                }
                            }
                        }
                    }
                    // Left: cycle reverse / toggle
                    KeyCode::Left => {
                        let row_type = &CONFIG_ROWS[sel].1;
                        match row_type {
                            RowType::Toggle => toggle_row(sel, &yolo, &auto_compact, &show_diff),
                            RowType::Cycle(opts) => {
                                cycle_backward(sel, opts, &perm_idx, &model_idx, &provider_idx)
                            }
                            RowType::Text => {} // no-op on text rows
                        }
                    }
                    // Right: cycle forward / toggle
                    KeyCode::Right => {
                        let row_type = &CONFIG_ROWS[sel].1;
                        match row_type {
                            RowType::Toggle => toggle_row(sel, &yolo, &auto_compact, &show_diff),
                            RowType::Cycle(opts) => {
                                cycle_forward(sel, opts, &perm_idx, &model_idx, &provider_idx)
                            }
                            RowType::Text => {} // no-op on text rows
                        }
                    }
                    // Backspace: delete last char on text rows
                    KeyCode::Backspace => {
                        let row_type = &CONFIG_ROWS[sel].1;
                        if matches!(row_type, RowType::Text) {
                            match sel {
                                ROW_THRESHOLD => {
                                    threshold.write().pop();
                                }
                                ROW_MAX_ITER => {
                                    max_iter.write().pop();
                                }
                                _ => {}
                            }
                        }
                    }
                    // Char: insert on text rows
                    KeyCode::Char(c) => {
                        let row_type = &CONFIG_ROWS[sel].1;
                        if matches!(row_type, RowType::Text) {
                            match sel {
                                ROW_THRESHOLD => threshold.write().push(c),
                                ROW_MAX_ITER => max_iter.write().push(c),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    // ---- Render ----
    let sel = *cursor.read();
    let yolo_val = *yolo.read();
    let auto_compact_val = *auto_compact.read();
    let show_diff_val = *show_diff.read();
    let perm_val = *perm_idx.read();
    let model_val = *model_idx.read();
    let provider_val = *provider_idx.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    for (i, (label, row_type)) in CONFIG_ROWS.iter().enumerate() {
        let is_active = i == sel;
        let cursor_mark = if is_active { "> " } else { "  " };
        let label_style = if is_active {
            Style::new().fg(theme::THINKING).bold()
        } else {
            Style::new().fg(theme::TEXT)
        };

        let value_line = match row_type {
            RowType::Toggle => {
                let val = match i {
                    ROW_YOLO => yolo_val,
                    ROW_AUTO_COMPACT => auto_compact_val,
                    ROW_SHOW_DIFF => show_diff_val,
                    _ => false,
                };
                let (on_text, off_text) = if val {
                    ("[ON]", " OFF")
                } else {
                    (" ON ", "[OFF]")
                };
                let on_style = if val {
                    Style::new().fg(theme::SAGE).bold()
                } else {
                    Style::new().fg(theme::MUTED)
                };
                let off_style = if val {
                    Style::new().fg(theme::MUTED)
                } else {
                    Style::new().fg(theme::ERROR).bold()
                };
                Line::from(vec![
                    Span::styled(cursor_mark, Style::new().fg(theme::THINKING)),
                    Span::styled(format!("{:<22}", label), label_style),
                    Span::styled(on_text, on_style),
                    Span::styled(" ", Style::new()),
                    Span::styled(off_text, off_style),
                ])
            }
            RowType::Cycle(options) => {
                let idx = match i {
                    ROW_PERMISSION => perm_val,
                    ROW_MODEL => model_val,
                    ROW_PROVIDER => provider_val,
                    _ => 0,
                };
                let mut spans = vec![
                    Span::styled(cursor_mark, Style::new().fg(theme::THINKING)),
                    Span::styled(format!("{:<22}", label), label_style),
                ];
                for (j, opt) in options.iter().enumerate() {
                    let display = if i == ROW_MODEL { MODEL_LABELS[j] } else { opt };
                    if j == idx {
                        spans.push(Span::styled(
                            format!("[{}]", display),
                            Style::new().fg(theme::SAGE).bold(),
                        ));
                    } else {
                        spans.push(Span::styled(
                            format!(" {}", display),
                            Style::new().fg(theme::MUTED),
                        ));
                    }
                    if j < options.len() - 1 {
                        spans.push(Span::styled(" ", Style::new()));
                    }
                }
                Line::from(spans)
            }
            RowType::Text => {
                let display = match i {
                    ROW_THRESHOLD => {
                        let v = threshold.read().clone();
                        if v.is_empty() {
                            String::from("(empty)")
                        } else {
                            v
                        }
                    }
                    ROW_MAX_ITER => {
                        let v = max_iter.read().clone();
                        if v.is_empty() {
                            String::from("(empty)")
                        } else {
                            v
                        }
                    }
                    _ => String::new(),
                };
                Line::from(vec![
                    Span::styled(cursor_mark, Style::new().fg(theme::THINKING)),
                    Span::styled(format!("{:<22}", label), label_style),
                    Span::styled(
                        display,
                        if is_active {
                            Style::new().fg(theme::ACCENT).bold()
                        } else {
                            Style::new().fg(theme::TEXT)
                        },
                    ),
                ])
            }
        };
        lines.push(value_line);
    }

    // Footer hints
    lines.push(Line::from(""));
    lines.push(
        Line::from("  j/k Navigate  Space Toggle  ←→ Cycle  Enter Edit  q Close").fg(theme::DIM),
    );

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Config ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(50),
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

// ---------------------------------------------------------------------------
// Event helpers
// ---------------------------------------------------------------------------

fn toggle_row(sel: usize, yolo: &State<bool>, auto_compact: &State<bool>, show_diff: &State<bool>) {
    match sel {
        ROW_YOLO => *yolo.write() = !*yolo.read(),
        ROW_AUTO_COMPACT => *auto_compact.write() = !*auto_compact.read(),
        ROW_SHOW_DIFF => *show_diff.write() = !*show_diff.read(),
        _ => {}
    }
}

fn cycle_forward(
    sel: usize,
    options: &[&str],
    perm_idx: &State<usize>,
    model_idx: &State<usize>,
    provider_idx: &State<usize>,
) {
    match sel {
        ROW_PERMISSION => *perm_idx.write() = (*perm_idx.read() + 1) % options.len(),
        ROW_MODEL => *model_idx.write() = (*model_idx.read() + 1) % options.len(),
        ROW_PROVIDER => *provider_idx.write() = (*provider_idx.read() + 1) % options.len(),
        _ => {}
    }
}

fn cycle_backward(
    sel: usize,
    options: &[&str],
    perm_idx: &State<usize>,
    model_idx: &State<usize>,
    provider_idx: &State<usize>,
) {
    let n = options.len();
    match sel {
        ROW_PERMISSION => *perm_idx.write() = (*perm_idx.read() + n - 1) % n,
        ROW_MODEL => *model_idx.write() = (*model_idx.read() + n - 1) % n,
        ROW_PROVIDER => *provider_idx.write() = (*provider_idx.read() + n - 1) % n,
        _ => {}
    }
}
