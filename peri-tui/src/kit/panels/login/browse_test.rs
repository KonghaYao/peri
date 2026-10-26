//! Browse 列表、删除确认门与配置候选回归。

use super::*;

/// 删除确认门绑定确认时的身份：确认期间列表刷新（每 tick 重派生）不改变待删目标。
///
/// 旧实现只存下标，`delete_provider(*cursor.read())` 会在刷新后删到另一个 provider。
#[test]
#[serial]
fn delete_confirms_the_provider_that_was_frozen() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);
    let _note = NotificationGuard::install();
    let _ = take_delete_requests();

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(4, move |panel| {
        if frame == 0 {
            // Ctrl+D：冻结光标处（下标 0 = alpha）的身份
            let opened = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            assert_eq!(opened, EventResult::Consumed);
            assert_eq!(
                panel.confirm_delete_target.read().clone(),
                Some("alpha".to_string()),
                "进入确认时必须冻结待删除身份"
            );
            // 列表刷新：下标 0 变成 beta
            *PROVIDER_LIST.state().write() = vec![provider("beta"), provider("alpha")];
        }
        if frame == 1 {
            let result = panel.event(key(KeyCode::Enter), TEST_AREA);
            tx.send((
                result,
                *panel.mode.read(),
                panel.confirm_delete_target.read().clone(),
            ))
            .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (result, mode, target) = rx.try_recv().expect("确认必须走真实事件入口");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::Browse, "确认后退出确认门");
    assert_eq!(target, None, "确认门是一次性的，目标随之清空");
    assert_eq!(
        take_delete_requests(),
        vec!["alpha".to_string()],
        "删除必须按确认时冻结的身份发出，而不是刷新后该下标上的 beta"
    );
}

/// 目标已消失（列表刷新 / 他处删除）时不发送请求、不误删其他项，并给出可见提示。
#[test]
#[serial]
fn delete_skips_request_and_hints_when_target_vanished() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);
    let _note = NotificationGuard::install();
    let _ = take_delete_requests();

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    mount_panel_at(4, move |panel| {
        if frame == 0 {
            let opened = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            assert_eq!(opened, EventResult::Consumed);
            // 目标从列表消失
            *PROVIDER_LIST.state().write() = vec![provider("beta")];
        }
        if frame == 1 {
            let result = panel.event(key(KeyCode::Enter), TEST_AREA);
            tx.send((result, *panel.mode.read()))
                .expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let (result, mode) = rx.try_recv().expect("确认必须走真实事件入口");
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(mode, LoginPanelMode::Browse);
    assert!(
        take_delete_requests().is_empty(),
        "目标已消失时不得发出删除请求（更不得改成删除当前下标上的 beta）"
    );
    let message = NOTIFICATION
        .state()
        .read()
        .as_ref()
        .map(|note| note.message.clone())
        .expect("目标消失必须给出可见提示");
    assert!(
        message.contains("alpha"),
        "提示必须指明确认时的目标身份，实际：{message}"
    );
}

/// 确认视图显示的是冻结身份，不随列表刷新换成当前下标上的 provider。
#[test]
#[serial]
fn confirm_view_shows_frozen_target_after_refresh() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);

    let mut frame = 0usize;
    let buf = mount_panel_at(4, move |panel| {
        if frame == 0 {
            let _ = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            // 刷新：下标 0 变成 beta，确认门目标必须仍是 alpha
            *PROVIDER_LIST.state().write() = vec![provider("beta"), provider("alpha")];
        }
        frame += 1;
    });

    let text = buffer_text(&buf);
    assert!(
        text.contains("Provider: alpha"),
        "确认视图必须显示冻结的目标身份：\n{text}"
    );
    assert!(
        !text.contains("Provider: beta"),
        "确认视图不得改成刷新后该下标上的 provider：\n{text}"
    );
}

/// 鼠标点击在确认门内等价于 Enter（click as enter），同样按冻结身份删除。
#[test]
#[serial]
fn click_confirms_frozen_target() {
    let _list = ProviderListGuard::install(vec![provider("alpha"), provider("beta")]);
    let _ = take_delete_requests();

    let mut frame = 0usize;
    mount_panel_at(3, move |panel| {
        if frame == 0 {
            let _ = panel.event(
                Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
                TEST_AREA,
            );
            *PROVIDER_LIST.state().write() = vec![provider("beta"), provider("alpha")];
        }
        if frame == 1 {
            let result = panel.event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 5,
                    row: 3,
                    modifiers: KeyModifiers::NONE,
                }),
                TEST_AREA,
            );
            assert_eq!(result, EventResult::Consumed);
        }
        frame += 1;
    });

    assert_eq!(
        take_delete_requests(),
        vec!["alpha".to_string()],
        "点击确认与 Enter 同语义：按冻结身份删除"
    );
}

/// 编辑状态异常（Edit 模式但没有编辑状态）时在渲染前回退：本帧渲染的就是 Browse 视图。
///
/// 旧实现在渲染体内 `*mode.write()`：本帧先渲染出 Internal error，违背 TUI-RENDER-001。
#[test]
#[serial]
fn edit_mode_without_state_renders_browse_in_the_same_frame() {
    let _list = ProviderListGuard::install(vec![provider("alpha")]);

    let mut frame = 0usize;
    let (tx, rx) = std::sync::mpsc::channel();
    let buf = mount_panel_at(2, move |panel| {
        if frame == 0 {
            *panel.mode.write() = LoginPanelMode::Edit;
            *panel.edit_state.write() = None;
        }
        if frame == 1 {
            tx.send(*panel.mode.read()).expect("测试结果通道仍打开");
        }
        frame += 1;
    });

    let mode = rx.try_recv().expect("末帧必须上报状态");
    assert_eq!(mode, LoginPanelMode::Browse, "状态异常必须回退到 Browse");
    let text = buffer_text(&buf);
    assert!(
        text.contains("providers configured"),
        "回退发生在渲染之前，本帧即渲染 Browse 视图：\n{text}"
    );
    assert!(
        !text.contains("Internal error"),
        "渲染段不得改写状态（回退由 effect 承担）：\n{text}"
    );
}

/// 含给定 provider 身份的宿主配置——只服务于删除候选的纯函数回归。
fn config_with_providers(ids: &[&str]) -> crate::config::PeriConfig {
    let mut cfg = crate::config::PeriConfig::default();
    cfg.config.providers = ids
        .iter()
        .map(|id| ProviderConfig {
            id: (*id).to_string(),
            provider_type: "anthropic".to_string(),
            ..Default::default()
        })
        .collect();
    cfg
}

/// 删除候选（纯函数）：目标存在才产出「移除后」的副本，且不改源配置。
///
/// 真实删除路径要进程级 `PERI_CONFIG_HANDLE`（测试进程不可安装），这里覆盖候选取舍
/// 这一唯一不依赖句柄的判定：无候选 = 调用方跳过落盘，重复删除不产生写入。
#[test]
fn delete_candidate_prunes_copy_only_when_target_exists() {
    let cfg = config_with_providers(&["alpha", "beta"]);

    let candidate = delete_candidate(&cfg, "alpha").expect("目标存在时必须产出候选");
    assert_eq!(
        candidate
            .config
            .providers
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        vec!["beta"],
        "候选移除的只能是目标身份"
    );
    assert_eq!(
        cfg.config
            .providers
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"],
        "候选构造在副本上进行，不得改动源配置"
    );
    assert!(
        delete_candidate(&cfg, "gamma").is_none(),
        "目标已不在配置里时无候选：调用方跳过落盘与发布"
    );
}
