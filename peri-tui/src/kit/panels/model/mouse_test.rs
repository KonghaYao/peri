//! 内联 contextWindow editor 与右栏 K/V 的鼠标语义回归。

use super::*;

#[test]
#[serial]
fn window_editor_mouse_save_and_cancel_share_keyboard_actions() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let before = handle.read().clone();
    let switched = switched();
    let click_handle = Arc::clone(&handle);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let mut editor = WindowEditState::enter(&click_handle.read(), "sonnet", None)
                .expect("Responses 档位必须有内联 editor");
            editor.text = "0".into();
            editor.cursor = 1;
            let layout = test_window_layout(&editor);
            let save_row = pane_click_row(TEST_AREA, &layout, PaneHit::Save, 0);
            *panel.window_editor.write() = Some(editor);

            let _ = panel.event(
                left_down(save_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &switched,
            );
            let mouse_error = panel
                .window_editor
                .read()
                .as_ref()
                .map(|editor| (editor.error.clone(), editor.input_blocked));

            // 同一非法值用键盘 Ctrl+S：错误与 editor 保留必须与鼠标一致。
            let mut editor = WindowEditState::enter(&click_handle.read(), "sonnet", None)
                .expect("Responses 档位必须有内联 editor");
            editor.text = "0".into();
            editor.cursor = 1;
            *panel.window_editor.write() = Some(editor);
            let _ = panel.event(
                key_with(KeyCode::Char('s'), KeyModifiers::CONTROL),
                TEST_AREA,
                &switched,
            );
            let key_error = panel
                .window_editor
                .read()
                .as_ref()
                .map(|editor| (editor.error.clone(), editor.input_blocked));

            let cancel_layout = test_window_layout(
                panel
                    .window_editor
                    .read()
                    .as_ref()
                    .expect("非法值必须保留 editor"),
            );
            let cancel_row = pane_click_row(TEST_AREA, &cancel_layout, PaneHit::Cancel, 0);
            let _ = panel.event(
                left_down(cancel_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &switched,
            );
            tx.send((mouse_error, key_error, panel.window_editor.read().is_none()))
                .unwrap();
        }
    });

    let (mouse_error, key_error, closed) = rx.try_recv().unwrap();
    assert_eq!(
        mouse_error, key_error,
        "窗口 Save 点击与键盘 Ctrl+S 必须同一校验结果"
    );
    let (error, _) = mouse_error.expect("Save 点击必须走与键盘相同的校验");
    assert!(error.is_some(), "非法窗口值不得绕过校验");
    assert!(closed, "Cancel 点击关闭 editor");
    assert!(handle.read().config == before.config, "非法值不得写入配置");
}

/// 设计 §7.8：Model 底部提供“管理 Provider”入口，经设置面板互斥规则打开 Login，
/// 并留下一次性 provider 身份；不复制配置编辑状态。
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

/// 点击右栏字段行 = 聚焦该字段并执行与键盘相同的行为；Provider / Model 与
/// 其它字段一样走即时保存，不再有订阅专属草稿。
#[test]
#[serial]
fn right_pane_field_click_focuses_and_runs_keyboard_action() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let before = handle.read().clone();
    let switched = switched();
    let click_switched = Rc::clone(&switched);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            // Context window 行：点击与 Enter/→ 相同，进入内联 editor。
            let context_row = TEST_AREA.y + 3 + FIELD_CONTEXT_1M as u16;
            let context_result = panel.event(
                left_down(context_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &click_switched,
            );
            let editor = panel
                .window_editor
                .read()
                .as_ref()
                .map(|editor| (editor.alias.clone(), editor.text.clone()));
            let context_focus = (*panel.right_focus.read(), *panel.right_cursor.read());
            let _ = panel.event(key(KeyCode::Esc), TEST_AREA, &click_switched);

            // Model 行：点击即聚焦并执行 →，与其它 provider 相同。
            let model_row = TEST_AREA.y + 3 + FIELD_MODEL as u16;
            let model_result = panel.event(
                left_down(model_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &click_switched,
            );
            let model_focus = (*panel.right_focus.read(), *panel.right_cursor.read());
            let model_no_editor = panel.window_editor.read().is_none();
            let _ = panel.event(key(KeyCode::Esc), TEST_AREA, &click_switched);

            // Provider 行：点击即聚焦并执行 →，与其它 provider 相同。
            let provider_row = TEST_AREA.y + 3 + FIELD_PROVIDER as u16;
            let provider_result = panel.event(
                left_down(provider_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &click_switched,
            );
            let provider_focus = (*panel.right_focus.read(), *panel.right_cursor.read());
            let provider_no_editor = panel.window_editor.read().is_none();
            let _ = panel.event(key(KeyCode::Esc), TEST_AREA, &click_switched);

            // Max tokens 行：普通即时保存路径。
            let max_tokens_row = TEST_AREA.y + 3 + FIELD_MAX_TOKENS as u16;
            let max_tokens_result = panel.event(
                left_down(max_tokens_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &click_switched,
            );
            let max_tokens_focus = (*panel.right_focus.read(), *panel.right_cursor.read());
            let _ = panel.event(key(KeyCode::Esc), TEST_AREA, &click_switched);

            // Effort 行：保持即时保存语义，不进入 editor。
            let effort_row = TEST_AREA.y + 3 + FIELD_EFFORT as u16;
            let effort_result = panel.event(
                left_down(effort_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &click_switched,
            );
            let effort_focus = (*panel.right_focus.read(), *panel.right_cursor.read());
            let no_editor = panel.window_editor.read().is_none();

            tx.send((
                context_result,
                editor,
                context_focus,
                model_result,
                model_focus,
                model_no_editor,
                provider_result,
                provider_focus,
                provider_no_editor,
                max_tokens_result,
                max_tokens_focus,
                effort_result,
                effort_focus,
                no_editor,
            ))
            .unwrap();
        }
    });

    let (
        context_result,
        editor,
        context_focus,
        model_result,
        model_focus,
        model_no_editor,
        provider_result,
        provider_focus,
        provider_no_editor,
        max_tokens_result,
        max_tokens_focus,
        effort_result,
        focus,
        no_editor,
    ) = rx.try_recv().unwrap();
    assert_eq!(context_result, EventResult::Consumed);
    assert_eq!(
        editor,
        Some(("sonnet".to_string(), "3000".to_string())),
        "点击 Responses context 行必须进入既有内联 editor"
    );
    assert_eq!(context_focus, (true, FIELD_CONTEXT_1M));
    assert_eq!(model_result, EventResult::Consumed);
    assert_eq!(model_focus, (true, FIELD_MODEL));
    assert!(model_no_editor, "Model 行点击不得打开专属面板");
    assert_eq!(provider_result, EventResult::Consumed);
    assert_eq!(provider_focus, (true, FIELD_PROVIDER));
    assert!(provider_no_editor, "Provider 行点击不得打开专属面板");
    assert_eq!(max_tokens_result, EventResult::Consumed);
    assert_eq!(
        max_tokens_focus,
        (true, FIELD_MAX_TOKENS),
        "Max tokens 行点击聚焦后沿用即时保存路径"
    );
    assert_eq!(effort_result, EventResult::Consumed);
    assert_eq!(
        focus,
        (true, FIELD_EFFORT),
        "普通字段点击只聚焦并保持原保存路径"
    );
    assert!(no_editor, "Effort 行不得进入 editor");
    assert!(
        handle.read().config == before.config,
        "点击本身不得写入配置（无 authority 时即时保存失败）"
    );
    assert!(switched.borrow().is_none(), "右栏点击不得切换 active alias");
}

/// 空白、边框、分隔线与滚动条不触发字段动作，也不打开 Login。
#[test]
#[serial]
fn right_pane_click_ignores_blank_border_scrollbar_and_divider() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let before = guard.handle.read().clone();
    let switched = switched();
    let click_switched = Rc::clone(&switched);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let content_column = right_content_column(TEST_AREA);
            let field_row = TEST_AREA.y + 3;
            let blank_row = field_row + FIELD_COUNT as u16 + 1;
            let hint_row = TEST_AREA.y + TEST_AREA.height - 2;
            for (row, column) in [
                (TEST_AREA.y, content_column),
                (TEST_AREA.y + TEST_AREA.height - 1, content_column),
                (field_row, TEST_AREA.x + TEST_AREA.width * 45 / 100),
                (field_row, TEST_AREA.x + TEST_AREA.width - 1),
                (blank_row, content_column),
                (hint_row, TEST_AREA.x + TEST_AREA.width - 3),
            ] {
                let result = panel.event(left_down(row, column), TEST_AREA, &click_switched);
                assert_eq!(
                    result,
                    EventResult::Consumed,
                    "区域内点击仍消费防穿透：({row}, {column})"
                );
            }
            tx.send((
                (*panel.right_focus.read(), *panel.right_cursor.read()),
                panel.window_editor.read().is_none(),
            ))
            .unwrap();
        }
    });

    let (focus, no_editor) = rx.try_recv().unwrap();
    assert_eq!(focus, (false, 0), "空白/边框/滚动条不得移动焦点");
    assert!(no_editor, "空白/边框/滚动条不得进入编辑");
    assert!(
        crate::kit::atoms::OPEN_PANELS.state().read().is_empty(),
        "按钮之外的底部提示不打开 Login"
    );
    assert!(guard.handle.read().config == before.config);
    assert!(switched.borrow().is_none());
}

/// 右栏滚动偏移后，点击仍然命中渲染出的那一行。
#[test]
#[serial]
fn right_pane_click_uses_scroll_offset() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let switched = switched();
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let offset = 2u16;
            panel
                .right_scroll
                .write()
                .set_offset(Position::new(0, offset));
            let row = TEST_AREA.y + 3 + FIELD_CONTEXT_1M as u16 - offset;
            let result = panel.event(
                left_down(row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &switched,
            );
            let editor = panel
                .window_editor
                .read()
                .as_ref()
                .map(|editor| editor.text.clone());
            tx.send((result, editor)).unwrap();
        }
    });

    let (result, editor) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(
        editor.as_deref(),
        Some("3000"),
        "滚动后点击可见行仍命中 context 字段"
    );
}
