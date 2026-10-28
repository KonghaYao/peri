//! 底部“管理 Provider”按钮与 Ctrl+L 的等价回归。

use super::*;

// ── 设计 §7.8：底部“管理 Provider”按钮 ─────────────────────────────────────

fn navigation_snapshot() -> (Vec<PanelKind>, Option<PanelKind>, Option<String>) {
    (
        crate::kit::atoms::OPEN_PANELS.state().read().clone(),
        *crate::kit::atoms::ACTIVE_PANEL.state().read(),
        crate::kit::atoms::LOGIN_PANEL_REQUEST
            .state()
            .read()
            .clone()
            .and_then(|request| request.provider_id),
    )
}

fn clear_navigation() {
    crate::kit::atoms::OPEN_PANELS.state().write().clear();
    *crate::kit::atoms::ACTIVE_PANEL.state().write() = None;
    *crate::kit::atoms::LOGIN_PANEL_REQUEST.state().write() = None;
}

/// 底部按钮点击与 Ctrl+L 打开同一面板并传递同一一次性目标身份。
#[test]
#[serial]
fn manage_provider_button_click_matches_ctrl_l() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let button_row = TEST_AREA.y + TEST_AREA.height - 2;
    let (tx, rx) = std::sync::mpsc::channel();

    let click_switched = switched();
    let click_tx = tx.clone();
    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let result = panel.event(
                left_down(button_row, TEST_AREA.x + 2),
                TEST_AREA,
                &click_switched,
            );
            click_tx.send((result, navigation_snapshot())).unwrap();
        }
    });
    let click = rx.try_recv().unwrap();

    clear_navigation();
    let key_switched = switched();
    let key_tx = tx;
    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let result = panel.event(
                key_with(KeyCode::Char('l'), KeyModifiers::CONTROL),
                TEST_AREA,
                &key_switched,
            );
            key_tx.send((result, navigation_snapshot())).unwrap();
        }
    });
    let keyboard = rx.try_recv().unwrap();

    assert_eq!(click, keyboard, "按钮点击必须与 Ctrl+L 等价");
    let (result, (open_panels, active, provider_id)) = click;
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(open_panels, vec![PanelKind::Login]);
    assert_eq!(active, Some(PanelKind::Login));
    assert_eq!(provider_id.as_deref(), Some("resp"));
}
