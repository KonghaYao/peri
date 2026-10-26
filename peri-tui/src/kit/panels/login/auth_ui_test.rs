//! Panel-level tests for the login authentication sub-views.

use super::*;
use crate::kit::panels::login::auth;
use crate::kit::panels::login::auth_render;
use crate::kit::panels::login::render;

// ── 认证子视图交互 ────────────────────────────────────────────────────────────

fn auth_target() -> crate::chatgpt_auth::AuthTarget {
    crate::chatgpt_auth::AuthTarget::new(std::env::temp_dir().join("peri-auth-test"), "work")
}

#[test]
#[serial]
fn auth_detail_escape_cancels_without_closing_login() {
    let _note = NotificationGuard::install();
    let _ = auth::take_test_dispatches();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(3, move |panel| {
        if frame == 0 {
            *panel.edit_state.write() = Some(LoginEditState::new_provider(
                &crate::config::PeriConfig::default(),
                None,
            ));
            *panel.mode.write() = LoginPanelMode::AuthDetail;
            panel
                .auth
                .write()
                .set_view_for_test(auth::AuthViewState::WaitingCallback {
                    url: "https://auth.example.test/authorize?state=synthetic&nonce=synthetic"
                        .into(),
                });
        }
        if frame == 1 {
            let result = panel.event(key(KeyCode::Esc), TEST_AREA);
            tx.send((result, *panel.mode.read())).unwrap();
        }
        frame += 1;
    });

    let (result, mode) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(
        mode,
        LoginPanelMode::Edit,
        "Esc 只取消认证等待，不能穿透关闭 Login"
    );
}

#[test]
#[serial]
fn confirm_logout_defaults_to_cancel_and_only_explicit_confirm_dispatches() {
    let _note = NotificationGuard::install();
    let _ = auth::take_test_dispatches();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(5, move |panel| {
        if frame == 0 {
            *panel.edit_state.write() = Some(LoginEditState::new_provider(
                &crate::config::PeriConfig::default(),
                None,
            ));
            *panel.mode.write() = LoginPanelMode::ConfirmLogout;
            assert!(
                panel
                    .auth
                    .write()
                    .open_logout_confirm(auth_target(), vec!["plan".to_string()],)
            );
        }
        if frame == 1 {
            let result = panel.event(key(KeyCode::Enter), TEST_AREA);
            let mode = *panel.mode.read();
            let dispatches = auth::take_test_dispatches();
            tx.send((result, mode, dispatches, 0usize)).unwrap();
            // 重新打开确认视图，验证显式确认才发注销。
            *panel.mode.write() = LoginPanelMode::ConfirmLogout;
            assert!(
                panel
                    .auth
                    .write()
                    .open_logout_confirm(auth_target(), vec!["plan".to_string()],)
            );
        }
        if frame == 2 {
            let _ = panel.event(key(KeyCode::Right), TEST_AREA);
            let _ = panel.event(key(KeyCode::Enter), TEST_AREA);
            let mode = *panel.mode.read();
            let dispatches = auth::take_test_dispatches();
            tx.send((EventResult::Consumed, mode, dispatches, 1usize))
                .unwrap();
        }
        frame += 1;
    });

    let (_, mode, dispatches, _) = rx.try_recv().unwrap();
    assert_eq!(mode, LoginPanelMode::Edit);
    assert!(
        dispatches.is_empty(),
        "默认焦点是取消，Enter 不得注销：{dispatches:?}"
    );
    let (_, mode, dispatches, _) = rx.try_recv().unwrap();
    assert_eq!(mode, LoginPanelMode::Edit);
    assert_eq!(dispatches, vec!["logout"]);
}

#[test]
#[serial]
fn auth_detail_buttons_hit_only_their_semantic_rows() {
    let _note = NotificationGuard::install();
    let _ = auth::take_test_dispatches();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(4, move |panel| {
        if frame == 0 {
            *panel.edit_state.write() = Some(LoginEditState::new_provider(
                &crate::config::PeriConfig::default(),
                None,
            ));
            *panel.mode.write() = LoginPanelMode::AuthDetail;
            panel
                .auth
                .write()
                .set_view_for_test(auth::AuthViewState::WaitingCallback {
                    url: "https://auth.example.test/authorize?state=synthetic".into(),
                });
        }
        if frame == 1 {
            let cancel_row = {
                let auth = panel.auth.read();
                let layout = auth_render::build_auth_detail_layout(
                    &auth,
                    TEST_AREA.width,
                    &render::LoginRenderColors::reset(),
                );
                layout
                    .buttons
                    .iter()
                    .find(|(_, action)| *action == auth::DetailAction::CancelLogin)
                    .map(|(row, _)| *row)
                    .unwrap()
            };
            // 空白行不触发注销/取消。
            let blank = panel.event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 3,
                    row: 2,
                    modifiers: KeyModifiers::NONE,
                }),
                TEST_AREA,
            );
            let still_detail = *panel.mode.read();
            // 取消按钮行执行取消。
            let cancel = panel.event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 3,
                    row: cancel_row as u16 + 1,
                    modifiers: KeyModifiers::NONE,
                }),
                TEST_AREA,
            );
            tx.send((blank, still_detail, cancel, *panel.mode.read()))
                .unwrap();
        }
        frame += 1;
    });
    let (blank, still_detail, cancel, mode) = rx.try_recv().unwrap();
    assert_eq!(blank, EventResult::Consumed);
    assert_eq!(still_detail, LoginPanelMode::AuthDetail, "空白行不能取消");
    assert_eq!(cancel, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::Edit);
}

#[test]
fn long_auth_url_wraps_and_keeps_button_rows_semantic() {
    let mut controller = auth::AuthController::new();
    let long = format!(
        "https://auth.example.test/authorize?state={}",
        "x".repeat(120)
    );
    controller.set_view_for_test(auth::AuthViewState::WaitingCallback { url: long });
    let layout =
        auth_render::build_auth_detail_layout(&controller, 36, &render::LoginRenderColors::reset());
    let url_rows = layout
        .lines
        .iter()
        .filter(|line| {
            line.spans.iter().any(|span| {
                let text = span.content.as_ref();
                text.contains("state=") || text.trim_start().starts_with('x')
            })
        })
        .count();
    assert!(url_rows > 1, "长 URL 必须换行，实际 {url_rows} 行");
    assert_eq!(
        layout.buttons.len(),
        3,
        "打开浏览器、复制链接、取消都必须有独立命中行"
    );
    let rows: Vec<usize> = layout.buttons.iter().map(|(row, _)| *row).collect();
    let mut sorted = rows.clone();
    sorted.sort_unstable();
    assert_eq!(rows, sorted);
    assert!(rows[0] > 0);
}

fn plan_edit_state() -> LoginEditState {
    let mut cfg = crate::config::PeriConfig::default();
    cfg.config
        .providers
        .push(peri_acp::provider::config::ProviderConfig {
            id: "plan".into(),
            provider_type: "openai_responses".into(),
            auth: Some(
                peri_acp::provider::config::ProviderAuthConfig::ChatGptPlan {
                    store: std::env::temp_dir().join("peri-auth-test"),
                    account: "work".into(),
                    subject: "old-subject".into(),
                    client_id: "old-client".into(),
                },
            ),
            models: Default::default(),
            ..Default::default()
        });
    LoginEditState::edit_provider(&cfg, "plan", None).unwrap()
}

#[test]
#[serial]
fn editing_plan_store_clears_the_previously_verified_identity() {
    let _note = NotificationGuard::install();
    let state = plan_edit_state();
    assert_eq!(state.plan.subject, "old-subject");

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(3, move |panel| {
        if frame == 0 {
            *panel.edit_state.write() = Some(state.clone());
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_focus.write() = LoginField::PlanStore;
        }
        if frame == 1 {
            let _ = panel.event(key(KeyCode::Char('x')), TEST_AREA);
            let state = panel.edit_state.read().clone().unwrap();
            tx.send((state.plan.store.clone(), state.plan.subject.clone()))
                .unwrap();
        }
        frame += 1;
    });
    let (store, subject) = rx.try_recv().unwrap();
    assert!(store.starts_with('x'), "store edit applied at the cursor");
    assert!(subject.is_empty(), "changed target cannot inherit identity");
}

#[test]
#[serial]
fn paste_on_auth_actions_is_ignored_without_panicking() {
    let _note = NotificationGuard::install();
    let state = plan_edit_state();
    let expected_store = state.plan.store.clone();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(3, move |panel| {
        if frame == 0 {
            *panel.edit_state.write() = Some(state.clone());
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_focus.write() = LoginField::AuthActions;
        }
        if frame == 1 {
            let result = panel.event(Event::Paste("evil".into()), TEST_AREA);
            let store = panel
                .edit_state
                .read()
                .as_ref()
                .map(|state| state.plan.store.clone());
            tx.send((result, store)).unwrap();
        }
        frame += 1;
    });
    let (result, store) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Ignored);
    assert_eq!(store.as_deref(), Some(expected_store.as_str()));
}

// ── 滚动后的子视图鼠标命中（P0-89）────────────────────────────────────────────

use crate::chatgpt_auth::{ChatGptAuthBackend, ChatGptAuthSession};
use crate::kit::atoms::NOTIFICATION;
use peri_acp::provider::auth::{AccountStatus, AuthError, AuthResult, LogoutOutcome};
use tokio_util::sync::CancellationToken;

/// 12 行窄面板：长 URL 换行后按钮落在语义行 12/13/14，滚动到 offset=5 仍可见。
const DETAIL_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 36,
    height: 12,
};

const LOGOUT_AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 40,
    height: 6,
};

fn long_auth_url() -> String {
    format!(
        "https://auth.example.test/authorize?state={}",
        "x".repeat(120)
    )
}

fn detail_colors() -> render::LoginRenderColors {
    render::LoginRenderColors::reset()
}

#[derive(Clone, Copy)]
enum DetailInput {
    Key(auth::DetailAction),
    Mouse(auth::DetailAction),
    Row(u16),
    Cell(u16, u16),
}

#[derive(Debug, PartialEq, Eq)]
struct DetailSnapshot {
    mode: LoginPanelMode,
    focus: LoginField,
    detail_focus: auth::DetailAction,
    notification: Option<String>,
    url_actions: Vec<(&'static str, String)>,
}

fn detail_button_screen_row(
    panel: &MountedPanel,
    action: auth::DetailAction,
    area: Rect,
    offset: u16,
) -> u16 {
    let layout =
        auth_render::build_auth_detail_layout(&panel.auth.read(), area.width, &detail_colors());
    let row = layout
        .buttons
        .iter()
        .find(|(_, candidate)| *candidate == action)
        .map(|(row, _)| *row as u16)
        .expect("按钮必须有语义行");
    assert!(
        row >= offset,
        "按钮必须在可见窗口内：row={row} offset={offset}"
    );
    area.y + 1 + (row - offset)
}

/// 在 12 行面板中从同一初始状态执行一次键盘或鼠标动作，返回可比较的快照。
fn run_detail_input(input: DetailInput, offset: u16) -> DetailSnapshot {
    let _note = NotificationGuard::install();
    *NOTIFICATION.state().write() = None;
    let _ = auth::take_test_url_actions();
    let url = long_auth_url();
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(2, move |panel| {
        if frame == 0 {
            *panel.edit_state.write() = Some(LoginEditState::new_provider(
                &crate::config::PeriConfig::default(),
                None,
            ));
            *panel.mode.write() = LoginPanelMode::AuthDetail;
            panel
                .auth
                .write()
                .set_view_for_test(auth::AuthViewState::WaitingCallback { url: url.clone() });
            if let DetailInput::Key(action) = input {
                panel.auth.write().set_detail_focus(action);
            }
            // ScrollView 在首帧初始化状态；偏移必须在同一帧设置后才能用于命中。
            panel.sv.write().set_offset(Position::new(0, offset));
            match input {
                DetailInput::Key(_) => {
                    let _ = panel.event(key(KeyCode::Enter), DETAIL_AREA);
                }
                DetailInput::Mouse(action) => {
                    let row = detail_button_screen_row(&panel, action, DETAIL_AREA, offset);
                    let _ = panel.event(click_at(row, 3), DETAIL_AREA);
                }
                DetailInput::Row(row) => {
                    let _ = panel.event(click_at(row, 3), DETAIL_AREA);
                }
                DetailInput::Cell(row, column) => {
                    let _ = panel.event(click_at(row, column), DETAIL_AREA);
                }
            }
            tx.send(DetailSnapshot {
                mode: *panel.mode.read(),
                focus: *panel.edit_focus.read(),
                detail_focus: panel.auth.read().detail_focus(),
                notification: NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .map(|note| note.message.clone()),
                url_actions: auth::take_test_url_actions(),
            })
            .unwrap();
        }
        frame += 1;
    });
    rx.try_recv().unwrap()
}

#[test]
#[serial]
fn scrolled_auth_detail_buttons_match_keyboard_actions() {
    for action in [
        auth::DetailAction::OpenBrowser,
        auth::DetailAction::CopyUrl,
        auth::DetailAction::CancelLogin,
    ] {
        let keyboard = run_detail_input(DetailInput::Key(action), 5);
        let mouse = run_detail_input(DetailInput::Mouse(action), 5);
        assert_eq!(
            mouse, keyboard,
            "滚动后鼠标与键盘必须执行同一动作：{action:?}"
        );
        match action {
            auth::DetailAction::OpenBrowser => assert_eq!(
                mouse.url_actions,
                vec![("open", long_auth_url())],
                "Open browser 必须打开当前授权 URL"
            ),
            auth::DetailAction::CopyUrl => assert_eq!(
                mouse.url_actions,
                vec![("copy", long_auth_url())],
                "Copy link 必须复制当前授权 URL"
            ),
            auth::DetailAction::CancelLogin => {
                assert_eq!(mouse.mode, LoginPanelMode::Edit, "Cancel sign-in 退回编辑");
                assert!(mouse.url_actions.is_empty());
            }
        }
    }
}

#[test]
#[serial]
fn scrolled_auth_detail_hit_ignores_blank_borders_and_hidden_buttons() {
    // offset=5：语义行 9（空行）对应屏幕行 5，点击不得执行动作。
    let blank = run_detail_input(DetailInput::Row(5), 5);
    assert_eq!(blank.mode, LoginPanelMode::AuthDetail);
    assert_eq!(blank.detail_focus, auth::DetailAction::OpenBrowser);
    assert!(blank.notification.is_none());
    assert!(blank.url_actions.is_empty());

    for border in [0, DETAIL_AREA.height - 1] {
        let hit = run_detail_input(DetailInput::Row(border), 5);
        assert_eq!(hit.mode, LoginPanelMode::AuthDetail, "边框不得命中按钮");
        assert!(hit.url_actions.is_empty());
    }

    // 同一边框列：按钮语义行的最左/最右列属于面板边框，不触发动作。
    for column in [DETAIL_AREA.x, DETAIL_AREA.x + DETAIL_AREA.width - 1] {
        let hit = run_detail_input(DetailInput::Cell(8, column), 5);
        assert_eq!(
            hit.mode,
            LoginPanelMode::AuthDetail,
            "边框列不得命中按钮：column={column}"
        );
        assert!(hit.url_actions.is_empty());
    }

    // offset=0：按钮在视口外，最后一个内容行（语义 9）不得误触。
    let hidden = run_detail_input(DetailInput::Row(DETAIL_AREA.height - 2), 0);
    assert_eq!(hidden.mode, LoginPanelMode::AuthDetail);
    assert!(hidden.url_actions.is_empty());
    assert!(hidden.notification.is_none());
}

#[test]
#[serial]
fn partially_scrolled_auth_detail_buttons_hit_only_visible_rows() {
    // offset=4：Open browser 屏幕行 9、Copy link 屏幕行 10 可见；
    // Cancel sign-in 的屏幕行 11 是下边框，不可见也不可命中。
    let open = run_detail_input(DetailInput::Mouse(auth::DetailAction::OpenBrowser), 4);
    assert_eq!(open.url_actions, vec![("open", long_auth_url())]);
    assert_eq!(open.mode, LoginPanelMode::AuthDetail);

    let copy = run_detail_input(DetailInput::Mouse(auth::DetailAction::CopyUrl), 4);
    assert_eq!(copy.url_actions, vec![("copy", long_auth_url())]);
    assert_eq!(copy.mode, LoginPanelMode::AuthDetail);

    let hidden_cancel = run_detail_input(DetailInput::Row(DETAIL_AREA.height - 1), 4);
    assert_eq!(hidden_cancel.mode, LoginPanelMode::AuthDetail);
    assert!(hidden_cancel.url_actions.is_empty());
}

#[test]
#[serial]
fn scrolled_logout_confirm_buttons_match_keyboard_actions() {
    let _note = NotificationGuard::install();
    let mut keyboard_frame = 0usize;
    let (keyboard_tx, keyboard_rx) = std::sync::mpsc::channel();
    mount_panel_at(2, move |panel| {
        if keyboard_frame == 0 {
            *panel.edit_state.write() = Some(LoginEditState::new_provider(
                &crate::config::PeriConfig::default(),
                None,
            ));
            *panel.mode.write() = LoginPanelMode::ConfirmLogout;
            assert!(
                panel
                    .auth
                    .write()
                    .open_logout_confirm(auth_target(), vec!["plan".to_string()])
            );
            let _ = auth::take_test_dispatches();
            let _ = panel.event(key(KeyCode::Right), LOGOUT_AREA);
            let _ = panel.event(key(KeyCode::Enter), LOGOUT_AREA);
            keyboard_tx
                .send((
                    *panel.mode.read(),
                    *panel.edit_focus.read(),
                    auth::take_test_dispatches(),
                ))
                .unwrap();
        }
        keyboard_frame += 1;
    });
    let keyboard = keyboard_rx.try_recv().unwrap();
    assert_eq!(keyboard.0, LoginPanelMode::Edit);
    assert_eq!(keyboard.2, vec!["logout"]);

    let mut mouse_frame = 0usize;
    let (mouse_tx, mouse_rx) = std::sync::mpsc::channel();
    mount_panel_at(2, move |panel| {
        if mouse_frame == 0 {
            *panel.edit_state.write() = Some(LoginEditState::new_provider(
                &crate::config::PeriConfig::default(),
                None,
            ));
            *panel.mode.write() = LoginPanelMode::ConfirmLogout;
            assert!(
                panel
                    .auth
                    .write()
                    .open_logout_confirm(auth_target(), vec!["plan".to_string()])
            );
            let _ = auth::take_test_dispatches();
            panel.sv.write().set_offset(Position::new(0, 5));
            let layout = {
                let auth = panel.auth.read();
                let confirm = auth.logout_confirm().unwrap();
                auth_render::build_logout_layout(confirm, &detail_colors())
            };
            let row = layout
                .buttons
                .iter()
                .find(|(_, focus)| *focus == auth::LogoutFocus::Confirm)
                .map(|(row, _)| *row as u16)
                .unwrap();
            assert!(row >= 5, "确认按钮必须落在滚动后的可见窗口");
            let _ = panel.event(click_at(1 + (row - 5), 3), LOGOUT_AREA);
            mouse_tx
                .send((
                    *panel.mode.read(),
                    *panel.edit_focus.read(),
                    auth::take_test_dispatches(),
                ))
                .unwrap();
        }
        mouse_frame += 1;
    });
    let mouse = mouse_rx.try_recv().unwrap();
    assert_eq!(mouse, keyboard, "滚动后点击确认必须与键盘注销一致");
}

// ── 注销终态：退出等待后仍送达且不污染后续操作 ────────────────────────────────

type LogoutTicket = crate::application::operations::OperationTicket<auth::HostConfigAuthScope>;

struct DetachedLogoutBackend;

impl ChatGptAuthBackend for DetachedLogoutBackend {
    fn open(
        &self,
        _target: &crate::chatgpt_auth::AuthTarget,
    ) -> AuthResult<Box<dyn ChatGptAuthSession>> {
        Ok(Box::new(DetachedLogoutSession))
    }
}

struct DetachedLogoutSession;

#[async_trait::async_trait]
impl ChatGptAuthSession for DetachedLogoutSession {
    async fn login(
        &self,
        _cancellation: CancellationToken,
        _on_url: Box<dyn FnOnce(String) + Send>,
    ) -> AuthResult<AccountStatus> {
        Err(AuthError::Unavailable)
    }

    async fn status(&self, _cancellation: CancellationToken) -> AuthResult<Option<AccountStatus>> {
        Ok(None)
    }

    async fn logout(&self, _cancellation: CancellationToken) -> AuthResult<LogoutOutcome> {
        Err(AuthError::Unavailable)
    }
}

fn test_auth_controller() -> auth::AuthController {
    auth::AuthController::with_backend(std::sync::Arc::new(DetachedLogoutBackend))
}

fn begin_logout_wait(panel: &MountedPanel) -> LogoutTicket {
    *panel.edit_state.write() = Some(plan_edit_state());
    *panel.mode.write() = LoginPanelMode::Edit;
    *panel.edit_focus.write() = LoginField::AuthActions;
    *panel.auth.write() = test_auth_controller();
    panel
        .auth
        .write()
        .sync_scope(auth::HostConfigAuthScope::new("test", auth_target()));
    assert!(
        panel
            .auth
            .write()
            .open_logout_confirm(auth_target(), vec!["plan".to_string()])
    );
    panel
        .auth
        .write()
        .prepare_logout_for_test()
        .expect("注销等待必须建立完成票")
}

#[derive(Debug, PartialEq, Eq)]
struct LogoutSnapshot {
    notification: Option<String>,
    mode: LoginPanelMode,
    unchecked: bool,
    starting_login: bool,
    logged_out: bool,
    logged_out_remote_revoked: Option<bool>,
}

fn logout_snapshot(panel: &MountedPanel) -> LogoutSnapshot {
    let view = panel.auth.read().view().clone();
    LogoutSnapshot {
        notification: NOTIFICATION
            .state()
            .read()
            .as_ref()
            .map(|note| note.message.clone()),
        mode: *panel.mode.read(),
        unchecked: matches!(&view, auth::AuthViewState::Unchecked),
        starting_login: matches!(&view, auth::AuthViewState::StartingLogin),
        logged_out: matches!(&view, auth::AuthViewState::LoggedOut(_)),
        logged_out_remote_revoked: match &view {
            auth::AuthViewState::LoggedOut(outcome) => Some(outcome.remote_revoked),
            _ => None,
        },
    }
}

fn push_logout_result(
    panel: &MountedPanel,
    ticket: LogoutTicket,
    result: AuthResult<LogoutOutcome>,
) {
    panel
        .auth
        .read()
        .push_event_for_test(ticket, auth::AuthEvent::LoggedOut(result));
}

/// 开始注销 → 等待中两次 Esc 回到 Browse（脱离等待）→ 后台终态送达。
fn run_detached_logout_case(
    result: impl Fn() -> AuthResult<LogoutOutcome> + 'static,
) -> (LogoutSnapshot, LogoutSnapshot) {
    let _note = NotificationGuard::install();
    *NOTIFICATION.state().write() = None;
    let mut ticket: Option<LogoutTicket> = None;
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(9, move |panel| {
        if frame == 0 {
            ticket = Some(begin_logout_wait(&panel));
        }
        if frame == 1 {
            let _ = panel.event(key(KeyCode::Esc), TEST_AREA);
            assert!(matches!(
                panel.auth.read().view(),
                auth::AuthViewState::CancellingLogout
            ));
            *NOTIFICATION.state().write() = None;
        }
        if frame == 2 {
            let _ = panel.event(key(KeyCode::Esc), TEST_AREA);
            assert_eq!(*panel.mode.read(), LoginPanelMode::Browse);
            assert!(panel.edit_state.read().is_none());
            assert!(matches!(
                panel.auth.read().view(),
                auth::AuthViewState::Unchecked
            ));
        }
        if frame == 3 {
            push_logout_result(&panel, ticket.clone().unwrap(), result());
        }
        if frame == 5 {
            tx.send(logout_snapshot(&panel)).unwrap();
            *NOTIFICATION.state().write() = None;
        }
        if frame == 6 {
            // 重复退出 / 重复终态事件都不得再次通知。
            panel.auth.write().shutdown();
            push_logout_result(
                &panel,
                ticket.clone().unwrap(),
                Ok(LogoutOutcome {
                    local_cleared: true,
                    remote_revoked: false,
                }),
            );
        }
        if frame == 8 {
            tx.send(logout_snapshot(&panel)).unwrap();
        }
        frame += 1;
    });
    (rx.recv().unwrap(), rx.recv().unwrap())
}

#[test]
#[serial]
fn detached_logout_local_clear_and_remote_unconfirmed_notify_once() {
    let (first, repeated) = run_detached_logout_case(|| {
        Ok(LogoutOutcome {
            local_cleared: true,
            remote_revoked: false,
        })
    });
    assert_eq!(
        first.notification,
        Some(crate::i18n::tr(
            "login-auth-logout-cleared-remote-unconfirmed"
        )),
        "本地已清除但远端未确认必须合并为一条如实提示"
    );
    assert_eq!(first.mode, LoginPanelMode::Browse);
    assert!(first.unchecked, "终态结果不得重新打开编辑视图");
    assert_eq!(repeated.notification, None, "重复退出/迟到结果不得重复通知");
    assert_eq!(repeated.mode, LoginPanelMode::Browse);
}

#[test]
#[serial]
fn detached_logout_local_failure_remote_revoked_reports_both_results() {
    let (first, repeated) = run_detached_logout_case(|| {
        Ok(LogoutOutcome {
            local_cleared: false,
            remote_revoked: true,
        })
    });
    assert_eq!(
        first.notification,
        Some(crate::i18n::tr(
            "login-auth-logout-uncleared-remote-revoked"
        )),
        "本地清理失败但远端已撤销时，必须同时显示两部分结果"
    );
    assert!(first.unchecked, "终态结果不得重新打开编辑视图");
    assert_eq!(repeated.notification, None, "重复退出/迟到结果不得重复通知");
}

#[test]
#[serial]
fn detached_logout_local_failure_reports_unknown_once() {
    let (first, repeated) = run_detached_logout_case(|| Err(AuthError::Unavailable));
    assert_eq!(
        first.notification,
        Some(crate::i18n::tr("login-auth-outcome-unknown")),
        "本地清理失败只能提示结果未知"
    );
    assert!(first.unchecked);
    assert_eq!(repeated.notification, None);
}

#[test]
#[serial]
fn detached_logout_result_does_not_pollute_next_login_or_logout() {
    let _note = NotificationGuard::install();
    *NOTIFICATION.state().write() = None;
    let mut ticket: Option<LogoutTicket> = None;
    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(11, move |panel| {
        if frame == 0 {
            ticket = Some(begin_logout_wait(&panel));
        }
        if frame == 1 {
            let _ = panel.event(key(KeyCode::Esc), TEST_AREA);
            let _ = panel.event(key(KeyCode::Esc), TEST_AREA);
        }
        if frame == 2 {
            push_logout_result(
                &panel,
                ticket.clone().unwrap(),
                Ok(LogoutOutcome {
                    local_cleared: true,
                    remote_revoked: true,
                }),
            );
        }
        if frame == 4 {
            let completed = logout_snapshot(&panel);
            assert_eq!(
                completed.notification,
                Some(crate::i18n::tr("login-auth-logout-cleared-remote-revoked"))
            );
            *NOTIFICATION.state().write() = None;
            // 下一次登录：旧终态事件不得改写新请求的状态。
            assert!(panel.auth.write().prepare_login().is_ok());
            assert!(matches!(
                panel.auth.read().view(),
                auth::AuthViewState::StartingLogin
            ));
            push_logout_result(
                &panel,
                ticket.clone().unwrap(),
                Ok(LogoutOutcome {
                    local_cleared: true,
                    remote_revoked: true,
                }),
            );
        }
        if frame == 6 {
            let next_login = logout_snapshot(&panel);
            tx.send(("next-login", next_login)).unwrap();
            // 下一次注销：旧代次不得把新注销标记为 detached。
            panel.auth.write().shutdown();
            // 真实的新注销发生在 Edit：保留草稿，避免终态落地后被 Browse 空闲清理覆盖。
            *panel.edit_state.write() = Some(plan_edit_state());
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_focus.write() = LoginField::AuthActions;
            assert!(
                panel
                    .auth
                    .write()
                    .open_logout_confirm(auth_target(), vec!["plan".to_string()])
            );
            let next_ticket = panel
                .auth
                .write()
                .prepare_logout_for_test()
                .expect("新注销必须建立完成票");
            push_logout_result(
                &panel,
                next_ticket,
                Ok(LogoutOutcome {
                    local_cleared: true,
                    remote_revoked: true,
                }),
            );
        }
        if frame == 8 {
            tx.send(("next-logout", logout_snapshot(&panel))).unwrap();
        }
        frame += 1;
    });
    let (_, next_login) = rx.recv().unwrap();
    assert_eq!(next_login.notification, None, "旧结果不得通知到新登录");
    assert!(next_login.starting_login, "旧结果不得改写新登录状态");
    assert_eq!(next_login.mode, LoginPanelMode::Browse);

    let (_, next_logout) = rx.recv().unwrap();
    assert_eq!(
        next_logout.notification,
        Some(crate::i18n::tr("login-auth-logout-cleared-remote-revoked")),
        "新注销必须按正常终态落回视图并提示"
    );
    assert!(next_logout.logged_out, "新注销结果必须写回视图");
    assert_eq!(next_logout.logged_out_remote_revoked, Some(true));
}

#[path = "auth_state_test.rs"]
mod auth_state;
