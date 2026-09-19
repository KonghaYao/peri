//! ratatui-kit ConfirmPopup component.
//!
//! 确认弹窗：从 `CONFIRM_PAYLOAD` atom 读取确认信息（title / message / details / pending_action），
//! Enter 执行确认，Esc 取消关闭。
//!
//! 同一文件另含 dirty 恢复专用确认（`DirtyRecoveryPopup`）：`RecoveryRequired`
//! 的风险解除不能复用「Enter 即确认」的通用语义，必须显式选择接受、默认取消，
//! 且确认内容未完整渲染时按取消收敛。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{layout::Constraint, style::Stylize, text::Line},
};

use crate::i18n;
use crate::kit::ask_user_action::AskUserResponseAction;
use crate::kit::atoms::{self, ASK_USER_RESPONSE_TX, CONFIRM_PAYLOAD, ConfirmAction, LANG_VERSION};
use crate::kit::panel_mouse::AreaTracker;
use crate::kit::popup_overlay::close_popup;
use peri_theme::atoms::THEME_ATOM;

/// 一次性选择与显示许可；不持有 client，不能重新选择当前会话。
#[derive(Debug)]
pub struct RecoveryConfirmation {
    pub target: peri_acp_types::workspace::RecoveryRequiredDetails,
    response: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<bool>>>,
    displayed: std::sync::atomic::AtomicBool,
}

impl RecoveryConfirmation {
    pub(crate) fn answer(&self, accepted: bool) {
        if let Some(tx) = self.response.lock().unwrap().take() {
            let visible = self.displayed.load(std::sync::atomic::Ordering::Acquire);
            let _ = tx.send(accepted && visible);
        }
    }

    /// 测试替身：模拟确认内容已在终端完整渲染。
    #[cfg(test)]
    pub(crate) fn mark_displayed(&self) {
        self.displayed
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// 弹窗被替换或撤销时，精确结清待决的 dirty 风险选择。
///
/// 这不是渲染兜底：确认可能在首帧渲染之前就被其他 popup 覆盖，此时
/// `RecoveryDisplay` 尚未建立、没有 Drop 可依赖。残留 payload 会一直持有响应
/// 通道，使等待用户回答的 load 永久占住 operation gate，因此替换/撤销边界
/// 必须显式按取消收敛。只处理 dirty 确认载荷，其他确认语义不变。
pub(crate) fn cancel_pending_dirty_recovery() -> bool {
    let state = CONFIRM_PAYLOAD.state();
    let mut payload = state.write();
    let pending = payload
        .as_ref()
        .is_some_and(|p| matches!(p.pending_action, ConfirmAction::RecoverDirty(_)));
    if !pending {
        return false;
    }
    let taken = payload.take().expect("dirty payload checked");
    drop(payload);
    if let ConfirmAction::RecoverDirty(owner) = taken.pending_action {
        owner.answer(false);
    }
    true
}

struct RecoveryPopupGuard(std::sync::Weak<RecoveryConfirmation>);
impl Drop for RecoveryPopupGuard {
    fn drop(&mut self) {
        let Some(owner) = self.0.upgrade() else {
            return;
        };
        owner.answer(false);
        // 先释放 payload 锁再动 POPUP_KIND——与 `confirm_dirty_recovery` 的
        // popup→payload 顺序保持单一方向，避免两个方向同时持锁。
        let cleared = {
            let state = CONFIRM_PAYLOAD.state();
            let mut payload = state.write();
            let mine = payload.as_ref().is_some_and(|p| {
                matches!(&p.pending_action,
                ConfirmAction::RecoverDirty(current) if std::sync::Arc::ptr_eq(current, &owner))
            });
            if mine {
                *payload = None;
            }
            mine
        };
        let still_open = { *atoms::POPUP_KIND.state().read() == Some(atoms::PopupKind::Confirm) };
        if cleared && still_open {
            *atoms::POPUP_KIND.state().write() = None;
        }
    }
}

pub(crate) async fn confirm_dirty_recovery(
    target: peri_acp_types::workspace::RecoveryRequiredDetails,
) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let owner = std::sync::Arc::new(RecoveryConfirmation {
        target: target.clone(),
        response: std::sync::Mutex::new(Some(tx)),
        displayed: std::sync::atomic::AtomicBool::new(false),
    });
    {
        let popup = atoms::POPUP_KIND.state();
        let mut popup = popup.write();
        let payload = CONFIRM_PAYLOAD.state();
        let mut payload = payload.write();
        if popup.is_some() || payload.is_some() {
            return false;
        }
        *payload = Some(atoms::ConfirmPayload {
            title: i18n::tr("dirty-recovery-title"),
            message: i18n::tr("dirty-recovery-risk"),
            details: vec![
                i18n::tr("dirty-recovery-responsibility"),
                format!("{} / generation {}", target.thread_id, target.generation),
            ],
            pending_action: ConfirmAction::RecoverDirty(owner.clone()),
        });
        *popup = Some(atoms::PopupKind::Confirm);
    }
    let _guard = RecoveryPopupGuard(std::sync::Arc::downgrade(&owner));
    drop(owner);
    rx.await.unwrap_or(false)
}

struct RecoveryDisplay {
    owner: std::sync::Arc<RecoveryConfirmation>,
    width: u16,
    height: u16,
    area: Option<ratatui_kit::ratatui::layout::Rect>,
}
impl RecoveryDisplay {
    fn record_area(&mut self, area: ratatui_kit::ratatui::layout::Rect) {
        let visible = area.width >= self.width && area.height >= self.height;
        self.area = visible.then_some(area);
        self.owner
            .displayed
            .store(visible, std::sync::atomic::Ordering::Release);
        if !visible {
            self.owner.answer(false);
        }
    }
}
impl Hook for RecoveryDisplay {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.record_area(drawer.area);
    }
}
impl Drop for RecoveryDisplay {
    fn drop(&mut self) {
        self.owner.answer(false);
    }
}

fn recovery_choice(
    event: &Event,
    selected: &mut bool,
    area: Option<ratatui_kit::ratatui::layout::Rect>,
) -> Option<bool> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press && key.modifiers.is_empty() => {
            match key.code {
                KeyCode::Esc => Some(false),
                KeyCode::Up | KeyCode::Down | KeyCode::Tab => {
                    *selected = !*selected;
                    None
                }
                KeyCode::Enter => Some(*selected),
                _ => None,
            }
        }
        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => area
            .filter(|rect| rect.contains((mouse.column, mouse.row).into()))
            .and_then(|rect| match mouse.row.checked_sub(rect.y) {
                Some(8) => Some(false),
                Some(9) => Some(true),
                _ => None,
            }),
        _ => None,
    }
}

#[derive(Default, Props)]
pub struct DirtyRecoveryPopupProps {
    pub owner: Option<std::sync::Arc<RecoveryConfirmation>>,
}

#[component]
pub fn DirtyRecoveryPopup(
    props: &DirtyRecoveryPopupProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let owner = props
        .owner
        .as_ref()
        .expect("recovery owner required")
        .clone();
    let theme = hooks.use_atom(&THEME_ATOM);
    let _lang = hooks.use_atom(&LANG_VERSION);
    let selected = hooks.use_state(|| false);
    let texts = vec![
        i18n::tr("dirty-recovery-title"),
        i18n::tr("dirty-recovery-risk"),
        i18n::tr("dirty-recovery-unknown"),
        i18n::tr("dirty-recovery-responsibility"),
        owner.target.thread_id.clone(),
        format!("generation {}", owner.target.generation),
        i18n::tr("dirty-recovery-hint"),
        format!(
            "{} {}",
            if !*selected.read() { ">" } else { " " },
            i18n::tr("dirty-recovery-cancel")
        ),
        format!(
            "{} {}",
            if *selected.read() { ">" } else { " " },
            i18n::tr("dirty-recovery-accept")
        ),
    ];
    let width = texts
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.as_str()))
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .min(u16::MAX as usize) as u16;
    let area = {
        let tracker = hooks.use_hook(|| RecoveryDisplay {
            owner: owner.clone(),
            width,
            height: 11,
            area: None,
        });
        tracker.width = width;
        tracker.area
    };
    let action_owner = owner.clone();
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            let mut choice = *selected.read();
            let answer = recovery_choice(&event, &mut choice, area);
            if choice != *selected.read() {
                *selected.write() = choice;
            }
            if let Some(accepted) = answer {
                action_owner.answer(accepted);
                close_popup();
            }
            // 风险选择期间禁止按键落入背景输入或快速切换快捷键。
            EventResult::Consumed
        },
    );
    let guard = theme.read();
    let lines: Vec<Line<'static>> = texts.into_iter().map(Line::from).collect();
    let paragraph = ratatui_kit::ratatui::widgets::Paragraph::new(lines)
        .style(ratatui_kit::ratatui::style::Style::new().fg(guard.semantic.text.primary))
        .block(ratatui_kit::ratatui::widgets::Block::default().borders(
            ratatui_kit::ratatui::widgets::Borders::TOP
                | ratatui_kit::ratatui::widgets::Borders::BOTTOM,
        ));
    element!(View(width: Constraint::Fill(1), height: Constraint::Fill(1)) { Text(text: paragraph) })
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

    // 确认动作：Enter 与鼠标左键点击共用（click as enter）
    let confirm = move || {
        // 执行确认逻辑
        if let Some(ref p) = *CONFIRM_PAYLOAD.state().read() {
            execute_confirm_action(&p.pending_action, |action| {
                if let Some(tx) = ASK_USER_RESPONSE_TX.get() {
                    let _ = tx.send(action);
                }
            });
        }
        // 清空确认弹窗 payload 并关闭弹窗
        *CONFIRM_PAYLOAD.state().write() = None;
        close_popup();
    };

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            // 鼠标：区域内左键点击 = 执行确认动作（click as enter）
            if let Event::Mouse(mouse) = event {
                if area.is_some() && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    confirm();
                    return EventResult::Consumed;
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
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    confirm();
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
        },
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
        // 通用确认动作绝不能代替专用风险选择。
        ConfirmAction::RecoverDirty(owner) => owner.answer(false),
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
#[path = "dirty_recovery_test.rs"]
mod recovery_tests;

#[cfg(test)]
#[path = "confirm_popup_test.rs"]
mod tests;
