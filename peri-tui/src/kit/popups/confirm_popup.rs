//! ratatui-kit ConfirmPopup component.
//!
//! 确认弹窗：从 `CONFIRM_PAYLOAD` atom 读取确认信息（title / message / details / pending_action），
//! Enter 执行确认，Esc 取消关闭。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{style::Stylize, text::Line},
};

use crate::kit::atoms::{self, CONFIRM_PAYLOAD, ConfirmAction};
use crate::kit::popup_overlay::close_popup;
use crate::kit::theme;

#[component]
pub fn ConfirmPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let payload_store = hooks.use_atom(&CONFIRM_PAYLOAD);
    let payload = payload_store.read().clone();
    let _ = payload_store;

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Enter) => {
                // 执行确认逻辑
                if let Some(ref p) = *CONFIRM_PAYLOAD.state().read() {
                    match &p.pending_action {
                        ConfirmAction::ThreadSwitch(target_id) => {
                            if let Some(tx) = atoms::THREAD_LOAD_TX.get() {
                                let _ = tx.send(target_id.clone());
                            }
                        }
                    }
                }
                *CONFIRM_PAYLOAD.state().write() = None;
                close_popup();
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    let popup_tokens = &theme::component().popup;
    let semantic = theme::semantic();
    let mut lines: Vec<Line<'_>> = Vec::new();

    match &payload {
        None => {
            lines.push(Line::from(""));
            lines.push(
                Line::from("  No pending confirmation.")
                    .fg(semantic.text.muted)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from("  Esc: close").fg(semantic.text.dim));
        }
        Some(p) => {
            lines.push(Line::from(""));
            lines.push(
                Line::from(format!("  {}", p.title))
                    .fg(popup_tokens.action_primary)
                    .bold(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(p.message.clone()).fg(semantic.text.primary));
            for detail in &p.details {
                lines.push(Line::from(detail.clone()).fg(semantic.text.muted));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("  Enter: confirm  Esc: cancel").fg(semantic.text.dim));
        }
    }

    let popup_block = ratatui_kit::ratatui::widgets::Block::default()
        .borders(
            ratatui_kit::ratatui::widgets::Borders::TOP
                | ratatui_kit::ratatui::widgets::Borders::BOTTOM,
        )
        .border_style(ratatui_kit::ratatui::style::Style::new().fg(popup_tokens.border))
        .title_top(
            Line::from(" Confirm ")
                .fg(popup_tokens.action_primary)
                .bold()
                .centered(),
        );
    let text_render = ratatui_kit::ratatui::widgets::Paragraph::new(
        ratatui_kit::ratatui::text::Text::from(lines),
    )
    .block(popup_block);

    element!(
        View(
            flex_direction: ratatui_kit::ratatui::layout::Direction::Vertical,
            width: ratatui_kit::ratatui::layout::Constraint::Fill(1),
            height: ratatui_kit::ratatui::layout::Constraint::Fill(1),
        ) {
            Text(text: text_render)
        }
    )
}
