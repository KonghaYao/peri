//! ratatui-kit ConfirmPopup component.
//!
//! 确认弹窗：从 `CONFIRM_PAYLOAD` atom 读取确认信息（title / message / details / pending_action），
//! Enter 执行确认，Esc 取消关闭。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::{style::Stylize, text::Line},
};

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::atoms::{
    self, ASK_USER_PENDING, ASK_USER_REQUEST_ID, ASK_USER_RESPONSE_TX, CONFIRM_PAYLOAD,
    LANG_VERSION, ConfirmAction,
};
use crate::kit::panel_registry::close_panel;
use crate::kit::popup_overlay::close_popup;
use peri_theme::atoms::THEME_ATOM;

#[component]
pub fn ConfirmPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let payload_store = hooks.use_atom(&CONFIRM_PAYLOAD);
    let payload = payload_store.read().clone();
    let _ = payload_store;

    hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
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
                        ConfirmAction::RejectAskUser => {
                            // 读取 request_id（必须在 close_panel 之前，因为 close_panel 可能清掉 atom）
                            if let Some(id_str) = ASK_USER_REQUEST_ID.state().read().clone()
                                && let Some(tx) = ASK_USER_RESPONSE_TX.get()
                            {
                                let _ = tx.send(AskUserResponseAction::Reject {
                                    request_id_str: id_str,
                                });
                            }
                            // 关闭面板并清空状态
                            close_panel(PanelKind::AskUser);
                            *ASK_USER_PENDING.state().write() = None;
                            *ASK_USER_REQUEST_ID.state().write() = None;
                        }
                    }
                }
                // 清空确认弹窗 payload 并关闭弹窗
                *CONFIRM_PAYLOAD.state().write() = None;
                close_popup();
                EventResult::Consumed
            }
            (KeyModifiers::NONE, KeyCode::Esc) => {
                // 用户选择返回继续作答
                *CONFIRM_PAYLOAD.state().write() = None;
                close_popup();
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });
    let _ = hooks.use_atom(&LANG_VERSION);

    let popup_tokens = &theme_def.read().component.popup;
    let guard = theme_def.read();
    let semantic = &guard.semantic;
    let mut lines: Vec<Line<'_>> = Vec::new();

    match &payload {
        None => {
            lines.push(Line::from(""));
            lines.push(
                Line::from(i18n::tr("popup-confirm-empty"))
                    .fg(semantic.text.muted)
                    .italic(),
            );
            lines.push(Line::from(""));
            lines.push(Line::from(i18n::tr("common-esc-close")).fg(semantic.text.dim));
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
            lines.push(
                Line::from(i18n::tr("popup-confirm-action-hint")).fg(semantic.text.dim),
            );
        }
    }

    let popup_block = ratatui_kit::ratatui::widgets::Block::default()
        .borders(
            ratatui_kit::ratatui::widgets::Borders::TOP
                | ratatui_kit::ratatui::widgets::Borders::BOTTOM,
        )
        .border_style(ratatui_kit::ratatui::style::Style::new().fg(popup_tokens.border))
        .title_top(
            Line::from(i18n::tr("popup-confirm-title"))
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
