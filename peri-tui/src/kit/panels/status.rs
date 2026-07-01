//! ratatui-kit StatusPanel component.
//!
//! Phase 6a: dual-tab (Cost / Context) display with tab switching
//! (use_state + use_local_events). Mock data; Phase 8 通过 Atom 注入真实
//! session 状态。

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

#[allow(dead_code)]
const TAB_COST: usize = 0;
#[allow(dead_code)]
const TAB_CONTEXT: usize = 1;

#[component]
fn StatusPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let active_tab = hooks.use_state(|| TAB_COST);

    hooks.use_local_events({
        let active_tab = active_tab;
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Left => {
                        *active_tab.write() = TAB_COST;
                    }
                    KeyCode::Right => {
                        *active_tab.write() = TAB_CONTEXT;
                    }
                    // Esc: Phase 8 通过 use_input_layer 实现模态面板关闭
                    _ => {}
                }
            }
        }
    });

    let tab = *active_tab.read();

    // ── Tab bar ──────────────────────────────────────────────────────
    let tab_bar = Paragraph::new(Line::from(vec![
        Span::styled(
            " Cost ",
            if tab == TAB_COST {
                Style::new().fg(theme::TEXT).bg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::MUTED)
            },
        ),
        Span::styled(
            " Context ",
            if tab == TAB_CONTEXT {
                Style::new().fg(theme::TEXT).bg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::MUTED)
            },
        ),
    ]));

    // ── Content ──────────────────────────────────────────────────────
    let content_lines: Vec<Line<'_>> = match tab {
        TAB_COST => vec![
            Line::from(vec![
                Span::styled("Provider: ", Style::new().fg(theme::MUTED)),
                Span::styled("Anthropic", Style::new().fg(theme::TEXT).bold()),
            ]),
            Line::from(vec![
                Span::styled("Model: ", Style::new().fg(theme::MUTED)),
                Span::styled(
                    "claude-sonnet-4-20250514",
                    Style::new().fg(theme::TEXT).bold(),
                ),
            ]),
            Line::from(""),
            Line::from("  Session cost data not available").fg(theme::MUTED),
        ],
        TAB_CONTEXT => vec![
            Line::from(vec![
                Span::styled("Context window: ", Style::new().fg(theme::MUTED)),
                Span::styled("200K tokens (standard)", Style::new().fg(theme::TEXT)),
            ]),
            Line::from(vec![
                Span::styled("Usage: ", Style::new().fg(theme::MUTED)),
                Span::styled("34.2k / 200k  (17.1%)", Style::new().fg(theme::SAGE)),
            ]),
            Line::from(vec![
                Span::styled("Compact ratio: ", Style::new().fg(theme::MUTED)),
                Span::styled("0.32 / 0.70", Style::new().fg(theme::TEXT)),
            ]),
            Line::from(""),
            Line::from("  Context usage data not available").fg(theme::MUTED),
        ],
        _ => vec![Line::from("  Unknown tab").fg(theme::MUTED)],
    };

    // ── Footer ───────────────────────────────────────────────────────
    let footer = Line::from("  ← →) Switch Tab  Esc) Close").fg(theme::DIM);

    let content = Paragraph::new(ratatui::text::Text::from({
        let mut all: Vec<Line> = Vec::new();
        all.push(Line::from("")); // spacer after tab bar (rendered as separate Text)
        all.extend(content_lines);
        all.push(Line::from(""));
        all.push(footer);
        all
    }));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Status ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(40),
            height: Constraint::Length(14),
        ) {
            Text(text: tab_bar)
            Text(text: content)
        }
    )
}
