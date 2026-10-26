//! 内联 contextWindow editor 与右栏 K/V 的鼠标语义回归。

use super::*;

#[test]
#[serial]
fn model_manage_provider_opens_login_with_one_shot_target() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let (tx, rx) = std::sync::mpsc::channel();
    let switched = switched();

    let buf = mount_panel_frames(TEST_AREA, 1, move |_, panel| {
        let result = panel.event(
            Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            TEST_AREA,
            &switched,
        );
        tx.send((
            result,
            crate::kit::atoms::OPEN_PANELS.state().read().clone(),
            *crate::kit::atoms::ACTIVE_PANEL.state().read(),
            crate::kit::atoms::LOGIN_PANEL_REQUEST
                .state()
                .read()
                .clone(),
        ))
        .unwrap();
    });

    let (result, open_panels, active, request) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(open_panels, vec![PanelKind::Login]);
    assert_eq!(active, Some(PanelKind::Login));
    assert_eq!(
        request.and_then(|request| request.provider_id).as_deref(),
        Some("resp"),
        "入口必须传递当前档位有效 provider 身份"
    );
    assert!(
        buffer_text(&buf).contains(&format!(
            "[{}]",
            i18n::tr("model-panel-provider-manage-hint")
        )),
        "底部必须渲染可点击的管理 Provider 按钮"
    );
}

// ── 设计 §7.9：普通右栏 K/V 行的鼠标命中 ────────────────────────────────────
