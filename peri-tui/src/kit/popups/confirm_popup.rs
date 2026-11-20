//! ratatui-kit ConfirmPopup component.
//!
//! 确认弹窗：从 `CONFIRM_PAYLOAD` atom 读取确认信息（title / message / details / pending_action），
//! Enter 执行确认，Esc 取消关闭。
//!
//! 「所见即所确认」：渲染帧把 payload 的版本与动作快照进事件闭包，确认时只有当前
//! payload 版本与快照一致才执行；两者之间 payload 被其它发布点替换则拒绝执行并提示，
//! 用户不会执行到自己没看到的动作。
use std::time::Duration;

use ratatui_kit::crossterm::event::Event;
use ratatui_kit::crossterm::event::KeyCode;
use ratatui_kit::crossterm::event::KeyEventKind;
use ratatui_kit::crossterm::event::KeyModifiers;
use ratatui_kit::crossterm::event::MouseButton;
use ratatui_kit::crossterm::event::MouseEvent;
use ratatui_kit::crossterm::event::MouseEventKind;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::style::Stylize;
use ratatui_kit::ratatui::text::Line;

use crate::i18n;
use crate::kit::ask_user_action::AskUserResponseAction;

use peri_tui_components::overlay::{OverlayFrame, OverlayHit, actions, hit};
use peri_tui_components::panel_mouse::AreaTracker;

use peri_theme::atoms::THEME_ATOM;

/// 渲染某帧时锁定的确认目标：该帧展示的 payload 版本 + 动作。
///
/// 事件闭包随渲染帧重建，闭包持有的是「用户这一帧看到的」内容；确认时用版本校验
/// 它仍是当前 payload，从而把「用户看到的动作」与「执行的动作」绑在一起。
#[derive(Debug, Clone)]
pub(crate) struct RenderedConfirm {
    version: u64,
    action: ConfirmAction,
    /// 返回按钮的命中区；行相对弹窗上沿，列相对弹窗左沿。测试快照可为空。
    back_hit: Option<BackHit>,
}

#[derive(Debug, Clone, Copy)]
struct BackHit {
    row: u16,
    start: u16,
    end: u16,
}

impl RenderedConfirm {
    #[must_use]
    fn from_payload(payload: &ConfirmPayload) -> Self {
        Self {
            version: payload.version,
            action: payload.pending_action.clone(),
            back_hit: None,
        }
    }

    #[must_use]
    fn with_back_hit(mut self, row: u16, back: (u16, u16)) -> Self {
        self.back_hit = Some(BackHit {
            row,
            start: back.0,
            end: back.1,
        });
        self
    }

    fn hits_back(&self, mouse: &MouseEvent, area: Rect) -> bool {
        let Some(back) = self.back_hit else {
            return false;
        };
        mouse.row == area.y.saturating_add(back.row)
            && (area.x.saturating_add(back.start)..area.x.saturating_add(back.end))
                .contains(&mouse.column)
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
            // 先关闭当前确认，再入队；消费者可能立即发布下一次确认。
            *CONFIRM_PAYLOAD.state().write() = None;
            crate::kit::popup_overlay::close_popup();
            execute_confirm_action(&action, |action| {
                if let Some(tx) = ASK_USER_RESPONSE_TX.get() {
                    let _ = tx.send(action);
                }
            });
        }
        // 不执行、不关闭：下一帧渲染新 payload，用户可对新内容再确认。
        ConfirmOutcome::Superseded => notify_superseded(),
        ConfirmOutcome::Empty => {
            *CONFIRM_PAYLOAD.state().write() = None;
            crate::kit::popup_overlay::close_popup();
        }
    }
    EventResult::Consumed
}

/// 动作行在弹窗正文中的下标：空行、标题、空行、message、空行，再接 details。
/// 渲染与点击命中都从这里取行号，避免两边偏移漂移。
fn action_row_index(payload: &ConfirmPayload) -> usize {
    5 + payload.details.len()
}

/// 确认弹窗事件判定——组件只做注册；渲染快照由调用方按帧注入（测试直接投喂）。
fn handle_confirm_event(
    event: &Event,
    rendered: Option<&RenderedConfirm>,
    area: Option<Rect>,
) -> EventResult {
    // 鼠标：区域外忽略（框架 hit_test 已过滤，这里按共享边界再判定一次）；
    // 内区与上下边框都是「click as enter」。
    if let Event::Mouse(mouse) = event {
        let Some(area) = area else {
            return EventResult::Ignored;
        };
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && hit(mouse, area) != OverlayHit::Outside
        {
            // 返回按钮与 Esc 同义；其余内区/边框仍是 click as enter。
            if rendered.is_some_and(|rendered| rendered.hits_back(mouse, area)) {
                return dismiss();
            }
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
        (KeyModifiers::NONE, KeyCode::Esc) => dismiss(),
        _ => EventResult::Ignored,
    }
}

/// 返回继续作答：清空待确认项并关闭弹窗。
fn dismiss() -> EventResult {
    *CONFIRM_PAYLOAD.state().write() = None;
    crate::kit::popup_overlay::close_popup();
    EventResult::Consumed
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
    let _ = hooks.use_atom(&LANG_VERSION);
    let guard = theme_def.read();
    let popup_tokens = &guard.component.popup;
    let semantic = &guard.semantic;
    let action_row = payload.as_ref().map(|_| {
        actions(
            &i18n::tr("popup-confirm-btn-confirm"),
            &i18n::tr("popup-confirm-btn-back"),
            &guard,
        )
    });
    let action_row_index = payload.as_ref().map(action_row_index);
    let rendered = payload.as_ref().map(|payload| {
        let rendered = RenderedConfirm::from_payload(payload);
        match (action_row_index, action_row.as_ref()) {
            (Some(index), Some(row)) => rendered.with_back_hit((index + 1) as u16, row.back),
            _ => rendered,
        }
    });

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| handle_confirm_event(&event, rendered.as_ref(), area),
    );

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
            // 确认/返回与真实点击共用一个动作行：按钮文本、样式和列区间同源。
            match &action_row {
                Some(row) => lines.push(row.line.clone()),
                None => lines
                    .push(Line::from(i18n::tr("popup-confirm-action-hint")).fg(semantic.text.dim)),
            }
        }
    }

    let frame = OverlayFrame::new(Line::from(i18n::tr("popup-confirm-title")))
        .border(Style::new().fg(popup_tokens.border))
        .title_style(Style::new().fg(popup_tokens.action_primary).bold());
    let text_render = ratatui_kit::ratatui::widgets::Paragraph::new(
        ratatui_kit::ratatui::text::Text::from(lines),
    )
    .block(frame.widget());

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
        ConfirmAction::ThreadSwitch {
            target_id,
            source_identity,
        } => {
            if let Some(tx) = atoms::THREAD_LOAD_TX.get() {
                let _ = tx.send_confirmed(target_id.clone(), source_identity.clone());
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

use crate::kit::atoms::ASK_USER_RESPONSE_TX;

use crate::kit::atoms::CONFIRM_PAYLOAD;

use crate::kit::atoms::ConfirmAction;

use crate::kit::atoms::ConfirmPayload;

use crate::kit::atoms::LANG_VERSION;

use crate::kit::atoms::NOTIFICATION;

use crate::kit::atoms;
use crate::kit::atoms::Notification;
