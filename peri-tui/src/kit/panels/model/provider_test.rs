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

/// 有未保存的内联 editor 时，按钮与 Ctrl+L 一样阻断并提示，不静默丢弃。
#[test]
#[serial]
fn manage_provider_button_click_keeps_unsaved_editor_blocked() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let switched = switched();
    let (tx, rx) = std::sync::mpsc::channel();
    let button_row = TEST_AREA.y + TEST_AREA.height - 2;

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let mut editor = WindowEditState::enter(&handle.read(), "sonnet", None)
                .expect("Responses 档位必须有内联 editor");
            editor.text = "9000".into();
            editor.cursor = 4;
            *panel.window_editor.write() = Some(editor);

            let click = panel.event(left_down(button_row, TEST_AREA.x + 2), TEST_AREA, &switched);
            let click_block = (
                click,
                crate::kit::atoms::NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .map(|notice| notice.message.clone()),
                navigation_snapshot(),
                panel.window_editor.read().is_some(),
            );

            let key_result = panel.event(
                key_with(KeyCode::Char('l'), KeyModifiers::CONTROL),
                TEST_AREA,
                &switched,
            );
            let key_block = (
                key_result,
                crate::kit::atoms::NOTIFICATION
                    .state()
                    .read()
                    .as_ref()
                    .map(|notice| notice.message.clone()),
                navigation_snapshot(),
                panel.window_editor.read().is_some(),
            );

            tx.send((click_block, key_block)).unwrap();
        }
    });

    let (click_block, key_block) = rx.try_recv().unwrap();
    let expected_notice = Some(i18n::tr("model-panel-editor-exit-first"));
    assert_eq!(click_block.0, EventResult::Consumed);
    assert_eq!(click_block.1, expected_notice);
    assert!(click_block.2.0.is_empty() && click_block.2.1.is_none());
    assert!(click_block.3, "点击按钮不得静默丢弃内联 editor");
    assert_eq!(click_block, key_block, "按钮与 Ctrl+L 的阻断结果一致");
}
