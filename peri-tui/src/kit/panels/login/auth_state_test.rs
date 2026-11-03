//! 认证状态落地：`Checking` 保存不写红字，状态成功后自动采用并清陈旧错误。
//!
//! 作为 `auth_ui` 测试的子模块，复用其认证夹具与面板挂载探针。

use std::time::UNIX_EPOCH;

use super::super::{NotificationGuard, TEST_AREA, key, mount_panel_at};
use super::{plan_edit_state, test_auth_controller};
use crate::chatgpt_auth::AccountStatus;
use crate::kit::atoms::NOTIFICATION;
use crate::kit::panels::login::LoginPanelMode;
use crate::kit::panels::login::auth::{AuthAction, AuthEvent};
use crate::kit::panels::login::auth_ui::apply_auth_effects;
use crate::kit::panels::login::edit_handler::auth_scope;
use crate::kit::panels::login::edit_state::{
    LoginEditState, LoginError, LoginErrorKind, LoginField,
};
use ratatui_kit::crossterm::event::KeyCode;
use serial_test::serial;

fn vacant_plan_state() -> LoginEditState {
    let mut state = plan_edit_state();
    state.plan.subject.clear();
    state.plan.client_id.clear();
    state
}

fn verified_status() -> AccountStatus {
    AccountStatus {
        account_ref: "work".into(),
        subject: "verified-subject".into(),
        email: Some("verified@example.test".into()),
        client_id: "verified-client".into(),
        plan_enabled: true,
        expires_at: peri_time::now_wall()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(60)
            .saturating_sub(60),
    }
}

#[test]
#[serial]
fn checking_save_reports_wait_then_status_success_adopts_and_clears_stale_error() {
    let _note = NotificationGuard::install();
    *NOTIFICATION.state().write() = None;
    let state = vacant_plan_state();
    let mut ticket = None;
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(2, move |panel| {
        if frame == 0 {
            *panel.edit_state.write() = Some(state.clone());
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_focus.write() = LoginField::Confirm;
            *panel.auth.write() = test_auth_controller();
            panel.auth.write().sync_scope(auth_scope(&state));
            ticket = Some(
                panel
                    .auth
                    .write()
                    .prepare_status(false)
                    .expect("status request")
                    .ticket,
            );
            // 模拟上一轮留下的陈旧身份错误；状态成功或保存门必须能清掉。
            panel.edit_state.write().as_mut().unwrap().inline_error =
                Some(LoginError::identity_required(
                    LoginField::AuthActions,
                    crate::i18n::tr("login-auth-identity-required"),
                ));
        }
        if frame == 1 {
            let _ = panel.event(key(KeyCode::Enter), TEST_AREA);
            let checking = (
                *panel.mode.read(),
                panel
                    .edit_state
                    .read()
                    .as_ref()
                    .and_then(|state| state.inline_error.as_ref().map(|error| error.kind)),
                NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .map(|note| note.message.clone()),
            );

            let ticket = ticket.take().expect("status ticket");
            panel.auth.write().push_event_for_test(
                ticket,
                AuthEvent::StatusChecked(Ok(Some(verified_status()))),
            );
            apply_auth_effects(
                panel.auth,
                panel.mode,
                panel.edit_state,
                panel.edit_focus,
                panel.edit_cursor,
            );
            let after_status = {
                let state = panel.edit_state.read().clone().unwrap();
                (
                    state.plan.subject.clone(),
                    state.plan.client_id.clone(),
                    state.inline_error.is_none(),
                    panel.auth.read().selected_action(&state.plan),
                )
            };

            *panel.edit_focus.write() = LoginField::Confirm;
            let _ = panel.event(key(KeyCode::Enter), TEST_AREA);
            let second_save = panel
                .edit_state
                .read()
                .as_ref()
                .and_then(|state| state.inline_error.as_ref().map(|error| error.kind));
            tx.send((checking, after_status, second_save)).unwrap();
        }
        frame += 1;
    });

    let (checking, after_status, second_save) = rx.try_recv().unwrap();
    assert_eq!(checking.0, LoginPanelMode::Edit);
    assert_eq!(
        checking.1, None,
        "Checking 保存不得写红字（inline_error 必须为空）"
    );
    assert_eq!(
        checking.2,
        Some(crate::i18n::tr("login-auth-identity-checking")),
        "Checking 只提示等待"
    );
    assert_eq!(after_status.0, "verified-subject");
    assert_eq!(after_status.1, "verified-client");
    assert!(after_status.2, "状态成功后陈旧身份错误必须清除");
    assert_eq!(
        after_status.3,
        Some(AuthAction::Status),
        "自动采用后动作必须复位到查询状态，不能滑到注销"
    );
    assert_ne!(
        second_save,
        Some(LoginErrorKind::IdentityRequired),
        "采用后再次保存不得再被身份阻断"
    );
}
