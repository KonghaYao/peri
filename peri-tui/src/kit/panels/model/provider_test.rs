//! 底部“管理 Provider”按钮与 Ctrl+L 的等价回归。

use super::EventResult;
use super::KeyCode;
use super::KeyModifiers;
use super::ModelPanelGuard;
use super::PanelKind;
use super::TEST_AREA;
use super::serial;

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
    guard.set_config(super::responses_config());
    let button_row = TEST_AREA.y + TEST_AREA.height - 2;
    let (tx, rx) = std::sync::mpsc::channel();

    let click_switched = super::switched();
    let click_tx = tx.clone();
    super::mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let result = panel.event(
                super::left_down(button_row, TEST_AREA.x + 2),
                TEST_AREA,
                &click_switched,
            );
            click_tx.send((result, navigation_snapshot())).expect("manage provider button click matches ctrl l: fixture channel must deliver its expected message");
        }
    });
    let click = rx.try_recv().expect("manage provider button click matches ctrl l: fixture channel must deliver its expected message");

    clear_navigation();
    let key_switched = super::switched();
    let key_tx = tx;
    super::mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let result = panel.event(
                super::key_with(KeyCode::Char('l'), KeyModifiers::CONTROL),
                TEST_AREA,
                &key_switched,
            );
            key_tx.send((result, navigation_snapshot())).expect("manage provider button click matches ctrl l: fixture channel must deliver its expected message");
        }
    });
    let keyboard = rx.try_recv().expect("manage provider button click matches ctrl l: fixture channel must deliver its expected message");

    assert_eq!(click, keyboard, "按钮点击必须与 Ctrl+L 等价");
    let (result, (open_panels, active, provider_id)) = click;
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(open_panels, vec![PanelKind::Login]);
    assert_eq!(active, Some(PanelKind::Login));
    assert_eq!(provider_id.as_deref(), Some("resp"));
}

/// 回归：底部按钮是通用 Button 的 Filled 外观——文本无方括号、样式反色，
/// 宽度与命中路径共用同一个 `ButtonVisual`。
#[test]
fn provider_button_is_filled_generic_button() {
    use ratatui_kit::ratatui::style::Modifier;

    let theme = peri_theme::builtin::dark_theme();
    let button = super::super::panel_render::provider_button_visual(&theme);
    assert!(
        !button.text.contains('[') && !button.text.contains(']'),
        "Filled 按钮不得再有方括号：{:?}",
        button.text
    );
    assert_eq!(
        usize::from(button.width),
        unicode_width::UnicodeWidthStr::width(button.text.as_str()),
        "命中宽度必须等于渲染文本宽度"
    );
    assert!(
        button.style.add_modifier.contains(Modifier::REVERSED),
        "Filled 按钮必须反色"
    );
    assert_eq!(
        button.style.fg,
        Some(theme.semantic.accent),
        "Primary tone 使用 accent 前景"
    );
}
