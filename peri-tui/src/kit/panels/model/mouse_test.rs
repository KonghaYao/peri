//! 内联 contextWindow editor 与右栏 K/V 的鼠标语义回归。

use super::Event;
use super::EventResult;
use super::KeyCode;
use super::KeyEvent;
use super::KeyModifiers;
use super::ModelPanelGuard;
use super::PanelKind;
use super::TEST_AREA;
use super::i18n;
use super::serial;
use super::switched;

#[test]
#[serial]
fn model_manage_provider_opens_login_with_one_shot_target() {
    let guard = ModelPanelGuard::install();
    guard.set_config(super::responses_config());
    let (tx, rx) = std::sync::mpsc::channel();
    let switched = switched();

    let buf = super::mount_panel_frames(TEST_AREA, 1, move |_, panel| {
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
        .expect("model manage provider opens login with one shot target: isolated fixture workspace must remain accessible");
    });

    let (result, open_panels, active, request) = rx.try_recv().expect("model manage provider opens login with one shot target: fixture channel must deliver its expected message");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(open_panels, vec![PanelKind::Login]);
    assert_eq!(active, Some(PanelKind::Login));
    assert_eq!(
        request.and_then(|request| request.provider_id).as_deref(),
        Some("resp"),
        "入口必须传递当前档位有效 provider 身份"
    );
    assert!(
        super::buffer_text(&buf).contains(&format!(
            " {} ",
            i18n::tr("model-panel-provider-manage-hint")
        )),
        "底部必须渲染可点击的管理 Provider 按钮（Filled，无方括号）"
    );
}

#[test]
#[serial]
fn manage_provider_targets_selected_profile_even_when_active_differs() {
    let guard = ModelPanelGuard::install();
    let mut config = super::responses_config();
    config.config.profiles.haiku.provider = "other".into();
    guard.set_config(config);
    let (tx, rx) = std::sync::mpsc::channel();
    super::mount_panel_frames(TEST_AREA, 2, move |frame, panel| {
        if frame == 0 {
            panel.event(super::key(KeyCode::Down), TEST_AREA, &switched());
            assert_eq!(&*panel.selected_alias.read(), "haiku");
            panel.event(
                super::key_with(KeyCode::Char('l'), KeyModifiers::CONTROL),
                TEST_AREA,
                &switched(),
            );
            tx.send(
                crate::kit::atoms::LOGIN_PANEL_REQUEST
                    .state()
                    .read()
                    .clone(),
            )
            .expect("manage provider targets selected profile even when active differs: isolated fixture workspace must remain accessible");
        }
    });
    assert_eq!(guard.handle.read().config.active_alias, "sonnet");
    assert_eq!(
        rx.recv()
            .expect("manage provider targets selected profile even when active differs: the fixture channel must exchange its expected message")
            .and_then(|request| request.provider_id)
            .as_deref(),
        Some("other")
    );
}

// ── 设计 §7.9：普通右栏 K/V 行的鼠标命中 ────────────────────────────────────
