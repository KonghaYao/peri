//! ratatui-kit ConfirmPopup component.
//!
//! 确认弹窗：从 `CONFIRM_PAYLOAD` atom 读取确认信息（title / message / details / pending_action），
//! Enter 执行确认，Esc 取消关闭。
//!
//! 「所见即所确认」：渲染帧把 payload 的版本与动作快照进事件闭包，确认时只有当前
//! payload 版本与快照一致才执行；两者之间 payload 被其它发布点替换则拒绝执行并提示，
//! 用户不会执行到自己没看到的动作。
use std::time::Duration;

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{layout::Rect, style::Stylize, text::Line},
};

use crate::i18n;
use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::atoms::{
    self, ASK_USER_RESPONSE_TX, CONFIRM_PAYLOAD, ConfirmAction, ConfirmPayload, LANG_VERSION,
    NOTIFICATION, Notification,
};
use crate::kit::panel_mouse::AreaTracker;
use crate::kit::popup_overlay::close_popup;
use peri_theme::atoms::THEME_ATOM;

/// 渲染某帧时锁定的确认目标：该帧展示的 payload 版本 + 动作。
///
/// 事件闭包随渲染帧重建，闭包持有的是「用户这一帧看到的」内容；确认时用版本校验
/// 它仍是当前 payload，从而把「用户看到的动作」与「执行的动作」绑在一起。
#[derive(Debug, Clone)]
pub(crate) struct RenderedConfirm {
    version: u64,
    action: ConfirmAction,
}

impl RenderedConfirm {
    fn from_payload(payload: &ConfirmPayload) -> Self {
        Self {
            version: payload.version,
            action: payload.pending_action.clone(),
        }
    }
}

/// 确认键（Enter / 鼠标点击）的判定结果。
#[derive(Debug)]
enum ConfirmOutcome {
    /// 版本一致：该动作就是用户看到的动作。
    Execute(ConfirmAction),
    /// 版本不一致：用户看到的 payload 已被替换，本次确认作废。
    Superseded,
    /// 当前无 payload：没有动作可执行。
    Empty,
}

/// 校验「渲染帧看到的 payload 版本」与「当前 payload 版本」是否一致。
fn resolve_confirm(
    rendered: Option<&RenderedConfirm>,
    current: Option<&ConfirmPayload>,
) -> ConfirmOutcome {
    match (rendered, current) {
        (Some(seen), Some(payload)) if payload.version == seen.version => {
            ConfirmOutcome::Execute(seen.action.clone())
        }
        // 有快照但已换人或已清空 → 用户看到的确认已失效。
        (Some(_), Some(_)) => ConfirmOutcome::Superseded,
        (Some(_), None) => ConfirmOutcome::Empty,
        // 渲染帧是空态（用户没看到任何动作），此后才出现的 payload 同样不算数。
        (None, Some(_)) => ConfirmOutcome::Superseded,
        (None, None) => ConfirmOutcome::Empty,
    }
}

/// 提示「确认内容已被替换」——本次确认作废，必须让用户看到，不能静默执行旧动作。
fn notify_superseded() {
    *NOTIFICATION.state().write() = Some(Notification {
        message: i18n::tr("popup-confirm-superseded"),
        until: peri_time::monotonic_now() + Duration::from_secs(2),
    });
}

/// 确认执行：版本一致才执行动作并关闭弹窗；被替换则保留弹窗并提示，等下一帧重新确认。
fn confirm(rendered: Option<&RenderedConfirm>) -> EventResult {
    let payload_atom = CONFIRM_PAYLOAD.state();
    let outcome = {
        let current = payload_atom.read();
        resolve_confirm(rendered, current.as_ref())
    };
    match outcome {
        ConfirmOutcome::Execute(action) => {
            execute_confirm_action(&action, |action| {
                if let Some(tx) = ASK_USER_RESPONSE_TX.get() {
                    let _ = tx.send(action);
                }
            });
            *CONFIRM_PAYLOAD.state().write() = None;
            close_popup();
        }
        // 不执行、不关闭：下一帧渲染新 payload，用户可对新内容再确认。
        ConfirmOutcome::Superseded => notify_superseded(),
        ConfirmOutcome::Empty => {
            *CONFIRM_PAYLOAD.state().write() = None;
            close_popup();
        }
    }
    EventResult::Consumed
}

/// 确认弹窗事件判定——组件只做注册；渲染快照由调用方按帧注入（测试直接投喂）。
fn handle_confirm_event(
    event: &Event,
    rendered: Option<&RenderedConfirm>,
    area: Option<Rect>,
) -> EventResult {
    // 鼠标：区域内左键点击 = 执行确认动作（click as enter）
    if let Event::Mouse(mouse) = event {
        if area.is_some() && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            return confirm(rendered);
        }
        return EventResult::Ignored;
    }
    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Enter) => confirm(rendered),
        (KeyModifiers::NONE, KeyCode::Esc) => {
            // 用户选择返回继续作答
            *CONFIRM_PAYLOAD.state().write() = None;
            close_popup();
            EventResult::Consumed
        }
        _ => EventResult::Ignored,
    }
}

#[component]
pub fn ConfirmPopup(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let payload_store = hooks.use_atom(&CONFIRM_PAYLOAD);
    let payload = payload_store.read().clone();
    let _ = payload_store;

    // 弹窗绘制区域（上一帧）——鼠标整窗点击 = 确认
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 本帧渲染的确认目标——与下面渲染用的是同一份 payload，事件闭包据此校验版本
    let rendered = payload.as_ref().map(RenderedConfirm::from_payload);

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| handle_confirm_event(&event, rendered.as_ref(), area),
    );
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
            lines.push(Line::from(i18n::tr("popup-confirm-action-hint")).fg(semantic.text.dim));
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
pub(crate) fn execute_confirm_action(
    action: &ConfirmAction,
    mut send_ask_user: impl FnMut(AskUserResponseAction),
) {
    match action {
        ConfirmAction::ThreadSwitch(target_id) => {
            if let Some(tx) = atoms::THREAD_LOAD_TX.get() {
                let _ = tx.send(target_id.clone());
            }
        }
        ConfirmAction::RejectAskUser {
            owner,
            request_id_json,
        } => {
            send_ask_user(AskUserResponseAction::Reject {
                owner: owner.clone(),
                request_id_str: request_id_json.clone(),
            });
            crate::kit::panel_registry::close_ask_user_panel_for_owner(owner);
        }
    }
}

#[cfg(test)]
#[path = "confirm_popup_test.rs"]
mod tests;
