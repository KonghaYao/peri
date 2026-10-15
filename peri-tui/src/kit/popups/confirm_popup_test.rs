//! ConfirmPopup 回归：「所见即所确认」的版本守卫与拒绝动作的 owner 匹配。
//!
//! 事件闭包随渲染帧重建，本测试用 `render_snapshot()` 复现「该帧闭包持有的确认目标」，
//! 再直接投喂事件入口——与 `panels/tasks/tests.rs` 投喂当帧快照的做法同源。

use super::*;
use crate::acp_client::InteractionOwner;
use crate::app::panel_types::PanelKind;
use crate::kit::acp_types::PendingInteraction;
use crate::kit::atoms::{ACTIVE_PANEL, ASK_USER_PENDING, OPEN_PANELS, POPUP_KIND, PopupKind};
use ratatui_kit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serial_test::serial;

/// 待决提问快照——`ASK_USER_PENDING` 的元素类型。
type AskUserPending = PendingInteraction<peri_acp_types::event_data::AskUser>;

/// 全局 atoms 的保存/恢复——断言失败也不污染其它测试。
struct ConfirmAtomsGuard {
    payload: Option<ConfirmPayload>,
    popup: Option<PopupKind>,
    ask_user: Option<AskUserPending>,
    open: Vec<PanelKind>,
    active: Option<PanelKind>,
    notification: Option<String>,
}

impl ConfirmAtomsGuard {
    fn install() -> Self {
        let previous = Self {
            payload: CONFIRM_PAYLOAD.state().read().clone(),
            popup: *POPUP_KIND.state().read(),
            ask_user: ASK_USER_PENDING.state().read().clone(),
            open: OPEN_PANELS.state().read().clone(),
            active: *ACTIVE_PANEL.state().read(),
            notification: notification_message(),
        };
        *CONFIRM_PAYLOAD.state().write() = None;
        *POPUP_KIND.state().write() = None;
        *ASK_USER_PENDING.state().write() = None;
        *OPEN_PANELS.state().write() = Vec::new();
        *ACTIVE_PANEL.state().write() = None;
        *NOTIFICATION.state().write() = None;
        previous
    }
}

impl Drop for ConfirmAtomsGuard {
    fn drop(&mut self) {
        *CONFIRM_PAYLOAD.state().write() = self.payload.take();
        *POPUP_KIND.state().write() = self.popup;
        *ASK_USER_PENDING.state().write() = self.ask_user.take();
        *OPEN_PANELS.state().write() = std::mem::take(&mut self.open);
        *ACTIVE_PANEL.state().write() = self.active;
        // Notification 非 Clone：按原消息重新发布。
        *NOTIFICATION.state().write() = self.notification.take().map(|message| Notification {
            message,
            until: peri_time::monotonic_now() + std::time::Duration::from_secs(6),
        });
    }
}

fn notification_message() -> Option<String> {
    NOTIFICATION
        .state()
        .read()
        .as_ref()
        .map(|notice| notice.message.clone())
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// 发布确认 payload 并打开确认弹窗（与生产发布点同路径）。
fn publish(payload: ConfirmPayload) {
    *CONFIRM_PAYLOAD.state().write() = Some(payload);
    crate::kit::popup_overlay::open_popup(PopupKind::Confirm);
}

/// 复现「本帧渲染锁定的确认目标」——组件渲染处同一份快照逻辑。
fn render_snapshot() -> Option<RenderedConfirm> {
    CONFIRM_PAYLOAD
        .state()
        .read()
        .as_ref()
        .map(RenderedConfirm::from_payload)
}

/// 登记待决提问并打开 AskUser 面板——RejectAskUser 执行与否由此可观。
fn ask_user_pending(owner: &InteractionOwner, request_id: &str) {
    *ASK_USER_PENDING.state().write() = Some(PendingInteraction {
        owner: owner.clone(),
        request_id_json: request_id.into(),
        payload: peri_acp_types::event_data::AskUser { questions: vec![] },
    });
    *OPEN_PANELS.state().write() = vec![PanelKind::AskUser];
    *ACTIVE_PANEL.state().write() = Some(PanelKind::AskUser);
}

fn reject_payload(owner: InteractionOwner, request_id: &str) -> ConfirmPayload {
    ConfirmPayload::new(
        i18n::tr("popup-confirm-reject-title"),
        i18n::tr("popup-confirm-reject-message"),
        vec![],
        ConfirmAction::RejectAskUser {
            owner,
            request_id_json: request_id.into(),
        },
    )
}

/// 待决请求是否仍未结清——RejectAskUser 执行会清掉匹配 owner 的请求并关闭面板。
fn reject_executed() -> bool {
    ASK_USER_PENDING.state().read().is_none()
}

/// [所见即所确认] 渲染后 payload 未变：Enter 执行用户看到的动作并关闭弹窗。
#[test]
#[serial]
fn test_confirm_executes_when_rendered_payload_unchanged() {
    let _guard = ConfirmAtomsGuard::install();
    let owner = InteractionOwner {
        token: 1,
        ..Default::default()
    };
    ask_user_pending(&owner, "A");
    publish(reject_payload(owner.clone(), "A"));

    let shown = render_snapshot();
    assert_eq!(
        handle_confirm_event(&key(KeyCode::Enter), shown.as_ref(), None),
        EventResult::Consumed
    );

    assert!(reject_executed(), "版本一致时必须执行用户看到的拒绝动作");
    assert_eq!(*POPUP_KIND.state().read(), None, "执行后弹窗应关闭");
    assert!(
        CONFIRM_PAYLOAD.state().read().is_none(),
        "执行后 payload 应清空"
    );
}

/// [所见即所确认] 渲染后 payload 被替换：Enter 不执行旧动作、不关闭弹窗、提示重新确认；
/// 下一帧渲染新 payload 后再确认才执行新动作。
#[test]
#[serial]
fn test_confirm_superseded_payload_is_not_executed() {
    let _guard = ConfirmAtomsGuard::install();
    let owner = InteractionOwner {
        token: 1,
        ..Default::default()
    };
    ask_user_pending(&owner, "A");
    publish(reject_payload(owner.clone(), "A"));
    let shown = render_snapshot();

    // 渲染与按键之间 payload 被替换（另一个发布点覆盖了本次确认）。
    publish(reject_payload(owner.clone(), "B"));

    assert_eq!(
        handle_confirm_event(&key(KeyCode::Enter), shown.as_ref(), None),
        EventResult::Consumed
    );
    assert!(!reject_executed(), "用户没看到的旧动作不能执行");
    assert_eq!(
        ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .map(|pending| pending.request_id_json.as_str()),
        Some("A"),
        "待决请求仍是原请求"
    );
    assert_eq!(
        *POPUP_KIND.state().read(),
        Some(PopupKind::Confirm),
        "确认作废时弹窗保持打开"
    );
    let superseded = i18n::tr("popup-confirm-superseded");
    assert_eq!(
        notification_message().as_deref(),
        Some(superseded.as_str()),
        "必须让用户看到本次确认已作废"
    );

    // 下一帧渲染新 payload 后，用户对新内容再确认——此时执行的是新动作。
    let shown_new = render_snapshot();
    assert_eq!(
        handle_confirm_event(&key(KeyCode::Enter), shown_new.as_ref(), None),
        EventResult::Consumed
    );
    assert!(reject_executed(), "对新 payload 确认后执行新动作");
    assert_eq!(*POPUP_KIND.state().read(), None, "执行后弹窗关闭");
}

/// Esc 仍是「清 payload + 关闭弹窗」，不执行任何动作。
#[test]
#[serial]
fn test_confirm_esc_clears_payload_without_action() {
    let _guard = ConfirmAtomsGuard::install();
    let owner = InteractionOwner {
        token: 1,
        ..Default::default()
    };
    ask_user_pending(&owner, "A");
    publish(reject_payload(owner.clone(), "A"));

    let shown = render_snapshot();
    assert_eq!(
        handle_confirm_event(&key(KeyCode::Esc), shown.as_ref(), None),
        EventResult::Consumed
    );

    assert!(!reject_executed(), "Esc 不执行拒绝动作");
    assert!(
        CONFIRM_PAYLOAD.state().read().is_none(),
        "Esc 应清空 payload"
    );
    assert_eq!(*POPUP_KIND.state().read(), None, "Esc 应关闭弹窗");
}

/// [回归测试] 确认弹窗冻结的 A rejection 不能关闭后来 active 的 B panel。
#[test]
#[serial]
fn test_confirm_reject_a_preserves_active_b_panel() {
    let old_pending = ASK_USER_PENDING.state().read().clone();
    let old_active = *ACTIVE_PANEL.state().read();
    let old_open = OPEN_PANELS.state().read().clone();
    *ASK_USER_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "B".into(),
        payload: peri_acp_types::event_data::AskUser { questions: vec![] },
    });
    *OPEN_PANELS.state().write() = vec![PanelKind::AskUser];
    *ACTIVE_PANEL.state().write() = Some(PanelKind::AskUser);
    let mut sent = None;
    execute_confirm_action(
        &ConfirmAction::RejectAskUser {
            owner: crate::acp_client::InteractionOwner {
                token: 1,
                ..Default::default()
            },
            request_id_json: "A".into(),
        },
        |action| sent = Some(action),
    );
    assert!(
        matches!(sent, Some(AskUserResponseAction::Reject { request_id_str, .. }) if request_id_str == "A")
    );
    assert_eq!(
        ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "B"
    );
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::AskUser));
    *ASK_USER_PENDING.state().write() = old_pending;
    *ACTIVE_PANEL.state().write() = old_active;
    *OPEN_PANELS.state().write() = old_open;
}
