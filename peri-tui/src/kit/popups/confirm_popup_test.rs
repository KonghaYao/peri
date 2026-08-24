use super::*;
use crate::app::panel_types::PanelKind;
use crate::kit::acp_types::PendingInteraction;
use crate::kit::atoms::{ACTIVE_PANEL, ASK_USER_PENDING, OPEN_PANELS};
use serial_test::serial;

/// [回归测试] 确认弹窗冻结的 A rejection 不能关闭后来 active 的 B panel。
#[test]
#[serial]
fn test_confirm_reject_a_preserves_active_b_panel() {
    let old_pending = ASK_USER_PENDING.state().read().clone();
    let old_active = *ACTIVE_PANEL.state().read();
    let old_open = OPEN_PANELS.state().read().clone();
    *ASK_USER_PENDING.state().write() = Some(PendingInteraction {
        request_id_json: "B".into(),
        payload: peri_acp_types::event_data::AskUser { questions: vec![] },
    });
    *OPEN_PANELS.state().write() = vec![PanelKind::AskUser];
    *ACTIVE_PANEL.state().write() = Some(PanelKind::AskUser);
    let mut sent = None;
    execute_confirm_action(
        &ConfirmAction::RejectAskUser {
            request_id_json: "A".into(),
        },
        |action| sent = Some(action),
    );
    assert!(
        matches!(sent, Some(AskUserResponseAction::Reject { request_id_str }) if request_id_str == "A")
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
