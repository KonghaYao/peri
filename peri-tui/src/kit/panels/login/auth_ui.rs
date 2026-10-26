//! Login authentication sub-view event actions.
//!
//! Kept separate from the panel owner so the component file stays within the
//! repository size budget. These functions only coordinate owner state and
//! dispatch prepared requests; credential I/O lives in `chatgpt_auth`.

use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui_kit::prelude::State;
use ratatui_kit::ratatui::layout::{Position, Rect};

use crate::kit::panels::login::auth::{self, AuthAction, AuthController, AuthEffect, LogoutFocus};
use crate::kit::panels::login::auth_policy;
use crate::kit::panels::login::auth_render;
use crate::kit::panels::login::edit_handler::{LoginEditIntent, auth_scope};
use crate::kit::panels::login::edit_state::{LoginEditState, LoginField};
use crate::kit::panels::login::render;

use super::{LoginEventCtx, LoginPanelMode};

// ── 认证子视图动作 ────────────────────────────────────────────────────────────

/// 把后台认证结果落到 owner 状态。只有通过完成门的事件才进入这里。
pub(super) fn apply_auth_effects(
    auth: State<AuthController>,
    mode: State<LoginPanelMode>,
    edit_state: State<Option<LoginEditState>>,
    edit_focus: State<LoginField>,
    edit_cursor: State<usize>,
) {
    let effects = {
        let mut auth_guard = auth.write();
        auth_guard.drain()
    };
    let had_effects = !effects.is_empty();
    let mut login_succeeded = false;
    for effect in effects {
        match effect {
            AuthEffect::LoginSucceeded => {
                // 身份写入统一交给下方 auth_policy：空/兼容 + 当前作用域才采用。
                login_succeeded = true;
                *edit_focus.write() = LoginField::AuthActions;
                *edit_cursor.write() = 0;
                *mode.write() = LoginPanelMode::Edit;
                auth::notify_key("login-auth-credentials-saved", 3);
            }
            AuthEffect::LoginFailed(failure) => {
                *edit_focus.write() = LoginField::AuthActions;
                *mode.write() = LoginPanelMode::Edit;
                auth::notify_key(failure.i18n_key(), 3);
            }
            AuthEffect::LoginCancelled => {
                *edit_focus.write() = LoginField::AuthActions;
                *mode.write() = LoginPanelMode::Edit;
                auth::notify_key("login-auth-ended", 3);
            }
            AuthEffect::LoggedOut(outcome) => {
                *edit_focus.write() = LoginField::AuthActions;
                *mode.write() = LoginPanelMode::Edit;
                auth::notify_logout_outcome(&outcome);
            }
            AuthEffect::LogoutUnknown => {
                *edit_focus.write() = LoginField::AuthActions;
                *mode.write() = LoginPanelMode::Edit;
                auth::notify_key("login-auth-outcome-unknown", 3);
            }
            // 用户已退出注销等待：只送达终态通知，不重开编辑视图。
            AuthEffect::LoggedOutDetached(outcome) => {
                auth::notify_logout_outcome(&outcome);
            }
            AuthEffect::LogoutUnknownDetached => {
                auth::notify_key("login-auth-outcome-unknown", 3);
            }
            AuthEffect::OutcomeUnknown(reason) => {
                tracing::debug!(?reason, "ChatGPT authentication outcome unknown");
                *edit_focus.write() = LoginField::AuthActions;
                *mode.write() = LoginPanelMode::Edit;
                auth::notify_key("login-auth-outcome-unknown", 3);
            }
            AuthEffect::StatusFailed(failure) => {
                auth::notify_key(failure.i18n_key(), 3);
            }
            AuthEffect::StatusChecked => {}
        }
    }
    if !had_effects {
        return;
    }

    // 统一的认证事实落地：清陈旧认证错误 + 对空/兼容身份静默采用。
    let adopted = {
        let auth_guard = auth.read();
        let mut edit_guard = edit_state.write();
        let Some(es) = edit_guard.as_mut() else {
            return;
        };
        es.clear_auth_error();
        if !es.is_plan_mode() {
            return;
        }
        let scope = auth_scope(es);
        auth_policy::adopt_if_verified(&mut es.plan, auth_guard.view(), auth_guard.scope(), &scope)
    };
    if adopted || login_succeeded {
        // 采用后 UseAccount 消失，旧的 index 会滑到注销；复位到“查询状态”。
        let plan = edit_state
            .read()
            .as_ref()
            .map(|es| es.plan.clone())
            .unwrap_or_default();
        auth.write().select_action(AuthAction::Status, &plan);
    }
}

pub(super) fn execute_edit_intent(ctx: &LoginEventCtx, intent: LoginEditIntent) {
    match intent {
        LoginEditIntent::None => {}
        LoginEditIntent::QueryStatus => {
            sync_auth_scope(ctx);
            if !auth::dispatch_status(ctx.auth, false) {
                let key = match ctx.auth.read().view() {
                    auth::AuthViewState::Failed(failure) => failure.i18n_key(),
                    _ => "login-auth-target-required",
                };
                auth::notify_key(key, 3);
            }
        }
        LoginEditIntent::Login => {
            sync_auth_scope(ctx);
            if auth::dispatch_login(ctx.auth) {
                ctx.sv.write().set_offset(Position::new(0, 0));
                *ctx.mode.write() = LoginPanelMode::AuthDetail;
            } else {
                let key = match ctx.auth.read().view() {
                    auth::AuthViewState::Failed(failure) => failure.i18n_key(),
                    _ => "login-auth-busy",
                };
                auth::notify_key(key, 3);
            }
        }
        LoginEditIntent::LogoutConfirm => {
            let prepared = {
                let edit_state = ctx.edit_state.read();
                let Some(es) = edit_state.as_ref() else {
                    return;
                };
                let scope = auth_scope(es);
                match scope.target() {
                    Some(target) => {
                        let providers = shared_plan_providers(es, &target);
                        Some((target, providers))
                    }
                    None => None,
                }
            };
            let Some((target, providers)) = prepared else {
                auth::notify_key("login-auth-target-required", 3);
                return;
            };
            sync_auth_scope(ctx);
            if ctx.auth.write().open_logout_confirm(target, providers) {
                *ctx.mode.write() = LoginPanelMode::ConfirmLogout;
            }
        }
    }
}

pub(super) fn sync_auth_scope(ctx: &LoginEventCtx) {
    if let Some(es) = ctx.edit_state.read().as_ref() {
        let scope = auth_scope(es);
        ctx.auth.write().sync_scope(scope);
    }
}

fn shared_plan_providers(
    es: &LoginEditState,
    target: &crate::chatgpt_auth::AuthTarget,
) -> Vec<String> {
    use peri_acp::provider::config::ProviderAuthConfig;
    es.baseline
        .config
        .providers
        .iter()
        .filter(|provider| {
            matches!(
                &provider.auth,
                Some(ProviderAuthConfig::ChatGptPlan { store, account, .. })
                    if store == &target.store && account == &target.account
            )
        })
        .map(|provider| provider.id.clone())
        .collect()
}

pub(super) fn handle_auth_detail_key(ctx: &LoginEventCtx, key: &KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Left | KeyCode::BackTab => {
            ctx.auth.write().cycle_detail(false);
        }
        KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
            ctx.auth.write().cycle_detail(true);
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let action = ctx.auth.read().detail_focus();
            execute_auth_detail(ctx, action);
        }
        KeyCode::Esc => {
            execute_auth_detail(ctx, auth::DetailAction::CancelLogin);
        }
        _ => {}
    }
    scroll_subview_focus(ctx.auth, ctx.mode, ctx.sv, ctx.area);
}

pub(super) fn execute_auth_detail(ctx: &LoginEventCtx, action: auth::DetailAction) {
    match action {
        auth::DetailAction::OpenBrowser => {
            if let Some(url) = ctx.auth.read().view().url().map(str::to_owned) {
                auth::open_browser(url);
            }
        }
        auth::DetailAction::CopyUrl => {
            if let Some(url) = ctx.auth.read().view().url().map(str::to_owned) {
                auth::copy_url(url);
            }
        }
        auth::DetailAction::CancelLogin => {
            if ctx.auth.write().cancel_login() {
                // 旧 completion ticket 已失效；对账用新票独立查询。
                auth::dispatch_status(ctx.auth, true);
            }
            *ctx.mode.write() = LoginPanelMode::Edit;
            *ctx.edit_focus.write() = LoginField::AuthActions;
            *ctx.edit_cursor.write() = 0;
        }
    }
}

pub(super) fn hit_auth_detail_button(
    ctx: &LoginEventCtx,
    mouse: &MouseEvent,
    area: Rect,
) -> Option<auth::DetailAction> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let layout =
        auth_render::build_auth_detail_layout(&ctx.auth.read(), area.width, &layout_colors());
    hit_button(ctx, mouse, area, &layout.buttons)
}

pub(super) fn handle_logout_confirm_key(ctx: &LoginEventCtx, key: &KeyEvent) {
    match key.code {
        KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
            if let Some(confirm) = ctx.auth.write().logout_confirm_mut() {
                confirm.focus = match confirm.focus {
                    LogoutFocus::Cancel => LogoutFocus::Confirm,
                    LogoutFocus::Confirm => LogoutFocus::Cancel,
                };
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let focus = ctx
                .auth
                .read()
                .logout_confirm()
                .map(|confirm| confirm.focus)
                .unwrap_or(LogoutFocus::Cancel);
            execute_logout_focus(ctx, focus);
        }
        KeyCode::Esc => {
            ctx.auth.write().clear_logout_confirm();
            *ctx.mode.write() = LoginPanelMode::Edit;
            *ctx.edit_focus.write() = LoginField::AuthActions;
        }
        _ => {}
    }
    scroll_subview_focus(ctx.auth, ctx.mode, ctx.sv, ctx.area);
}

pub(super) fn execute_logout_focus(ctx: &LoginEventCtx, focus: LogoutFocus) {
    match focus {
        LogoutFocus::Cancel => {
            ctx.auth.write().clear_logout_confirm();
        }
        LogoutFocus::Confirm => {
            if !auth::dispatch_logout(ctx.auth) {
                let key = match ctx.auth.read().view() {
                    auth::AuthViewState::Failed(failure) => failure.i18n_key(),
                    _ => "login-auth-outcome-unknown",
                };
                auth::notify_key(key, 3);
            }
        }
    }
    *ctx.mode.write() = LoginPanelMode::Edit;
    *ctx.edit_focus.write() = LoginField::AuthActions;
    *ctx.edit_cursor.write() = 0;
}

pub(super) fn hit_logout_button(
    ctx: &LoginEventCtx,
    mouse: &MouseEvent,
    area: Rect,
) -> Option<LogoutFocus> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let auth = ctx.auth.read();
    let confirm = auth.logout_confirm()?;
    let layout = auth_render::build_logout_layout(confirm, &layout_colors());
    hit_button(ctx, mouse, area, &layout.buttons)
}

fn hit_button<T: Copy>(
    ctx: &LoginEventCtx,
    mouse: &MouseEvent,
    area: Rect,
    buttons: &[(usize, T)],
) -> Option<T> {
    // 子视图使用 ScrollView：屏幕行先还原为内容语义行，再与按钮行比较。
    // 隐藏行/边框/面板外列不产生行号；滚动后按钮仍落在同一语义行。
    if mouse.column <= area.x || mouse.column >= area.x.saturating_add(area.width).saturating_sub(1)
    {
        return None;
    }
    let row = super::mouse_content_row(mouse, area, ctx.sv.read().offset().y)?;
    buttons
        .iter()
        .find(|(button_row, _)| *button_row == row)
        .map(|(_, action)| *action)
}

fn layout_colors() -> render::LoginRenderColors {
    render::LoginRenderColors::reset()
}

/// Keep the focused sub-view button inside the visible scroll window.
pub(super) fn scroll_subview_focus(
    auth_state: State<AuthController>,
    mode_state: State<LoginPanelMode>,
    sv_state: State<ratatui_kit::components::scroll_view::ScrollViewState>,
    area: Option<Rect>,
) {
    let Some(area) = area else {
        return;
    };
    let current_mode = *mode_state.read();
    let (row, total) = {
        let auth = auth_state.read();
        match current_mode {
            LoginPanelMode::AuthDetail => {
                let layout =
                    auth_render::build_auth_detail_layout(&auth, area.width, &layout_colors());
                let focus = auth.detail_focus();
                let row = layout
                    .buttons
                    .iter()
                    .find(|(_, action)| *action == focus)
                    .map(|(row, _)| *row)
                    .unwrap_or(0);
                (row, layout.lines.len())
            }
            LoginPanelMode::ConfirmLogout => {
                let Some(confirm) = auth.logout_confirm() else {
                    return;
                };
                let layout = auth_render::build_logout_layout(confirm, &layout_colors());
                let row = layout
                    .buttons
                    .iter()
                    .find(|(_, focus)| *focus == confirm.focus)
                    .map(|(row, _)| *row)
                    .unwrap_or(0);
                (row, layout.lines.len())
            }
            _ => return,
        }
    };
    let height = area.height.saturating_sub(2).max(1) as usize;
    let max_offset = total.saturating_sub(height);
    let mut sv = sv_state.write();
    let offset = sv.offset().y as usize;
    let next = next_scroll_offset(row, offset, height, max_offset);
    sv.set_offset(Position::new(0, next as u16));
}

/// Keep a focused content row inside `[offset, offset + height)`.
fn next_scroll_offset(row: usize, offset: usize, height: usize, max_offset: usize) -> usize {
    let next = if row < offset {
        row
    } else if row >= offset + height {
        row + 1 - height
    } else {
        offset
    };
    next.min(max_offset)
}

#[cfg(test)]
mod tests {
    use super::next_scroll_offset;

    #[test]
    fn focus_scrolls_up_down_and_clamps_to_content_end() {
        assert_eq!(next_scroll_offset(2, 5, 10, 40), 2);
        assert_eq!(next_scroll_offset(20, 0, 10, 40), 11);
        assert_eq!(next_scroll_offset(20, 15, 10, 40), 15);
        assert_eq!(next_scroll_offset(45, 0, 10, 40), 36);
    }
}
