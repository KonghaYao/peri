//! 档位草稿/内联 editor 与右栏 K/V 的鼠标语义回归。

use super::*;

#[test]
#[serial]
fn draft_mouse_cancel_discards_without_persisting() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let before = handle.read().clone();
    let switched = switched();
    let click_handle = Arc::clone(&handle);
    let click_switched = Rc::clone(&switched);

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let mut draft = responses_draft(&click_handle);
            draft.text = "128000".into();
            draft.cursor = draft.text.chars().count();
            let layout = test_profile_layout(&draft);
            let row = pane_click_row(TEST_AREA, &layout, PaneHit::Cancel, 0);
            *panel.profile_draft.write() = Some(draft);
            let result = panel.event(
                left_down(row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &click_switched,
            );
            assert_eq!(result, EventResult::Consumed);
        }
    });

    assert!(
        handle.read().config == before.config,
        "Cancel 必须放弃草稿且不落盘/不改内存配置"
    );
    assert!(switched.borrow().is_none(), "Cancel 不得切换 active alias");
}

#[test]
#[serial]
fn draft_mouse_save_does_not_bypass_validation() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let before = handle.read().clone();
    let switched = switched();
    let click_handle = Arc::clone(&handle);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let mut draft = responses_draft(&click_handle);
            draft.text = "12k".into();
            draft.cursor = draft.text.chars().count();
            let layout = test_profile_layout(&draft);
            let row = pane_click_row(TEST_AREA, &layout, PaneHit::Save, 0);
            *panel.profile_draft.write() = Some(draft);
            let result = panel.event(
                left_down(row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &switched,
            );
            let mouse_saved = panel
                .profile_draft
                .read()
                .as_ref()
                .map(|draft| (draft.error.clone(), draft.conflict, draft.input_blocked));

            // 同一非法草稿用键盘 Ctrl+S：结果必须与鼠标 Save 完全一致。
            let mut draft = responses_draft(&click_handle);
            draft.text = "12k".into();
            draft.cursor = draft.text.chars().count();
            *panel.profile_draft.write() = Some(draft);
            let _ = panel.event(
                key_with(KeyCode::Char('s'), KeyModifiers::CONTROL),
                TEST_AREA,
                &switched,
            );
            let key_saved = panel
                .profile_draft
                .read()
                .as_ref()
                .map(|draft| (draft.error.clone(), draft.conflict, draft.input_blocked));
            tx.send((result, mouse_saved, key_saved)).unwrap();
        }
    });

    let (result, mouse_saved, key_saved) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(
        mouse_saved, key_saved,
        "鼠标 Save 与键盘 Ctrl+S 必须同一结果"
    );
    let (error, conflict, _) = mouse_saved.expect("非法容量必须保留草稿");
    assert!(error.is_some(), "Save 点击必须显示与键盘相同的校验错误");
    assert!(!conflict, "字段校验错误不是 revision 冲突");
    assert!(
        handle.read().config == before.config,
        "校验失败不得改动内存配置"
    );
}

#[test]
#[serial]
fn draft_mouse_blank_border_and_scrollbar_do_not_act() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let before = handle.read().clone();
    let switched = switched();
    let click_handle = Arc::clone(&handle);
    let click_switched = Rc::clone(&switched);
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let draft = responses_draft(&click_handle);
            let focus = draft.focus;
            let layout = test_profile_layout(&draft);
            let save_row = pane_click_row(TEST_AREA, &layout, PaneHit::Save, 0);
            *panel.profile_draft.write() = Some(draft);

            // 标题行、左栏、分隔线、右栏滚动条列、底部提示行的按钮外区域都不产生动作。
            for (row, column) in [
                (TEST_AREA.y + 3, right_content_column(TEST_AREA)),
                (save_row, 2),
                (save_row, TEST_AREA.x + TEST_AREA.width * 45 / 100),
                (save_row, TEST_AREA.x + TEST_AREA.width - 1),
                (
                    TEST_AREA.y + TEST_AREA.height - 2,
                    TEST_AREA.x + TEST_AREA.width - 3,
                ),
            ] {
                let result = panel.event(left_down(row, column), TEST_AREA, &click_switched);
                assert_eq!(result, EventResult::Consumed, "区域内点击仍消费防穿透");
            }

            let preserved = panel.profile_draft.read().as_ref().map(|draft| draft.focus);
            tx.send((preserved, focus)).unwrap();
        }
    });

    let (preserved, focus) = rx.try_recv().unwrap();
    assert_eq!(preserved, Some(focus), "空白/边框/滚动条不得移动焦点");
    assert!(
        handle.read().config == before.config,
        "空白/边框/滚动条不得写入配置"
    );
    assert!(switched.borrow().is_none(), "不得切换 active alias");
}

#[test]
#[serial]
fn draft_mouse_hit_uses_right_scroll_offset() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let switched = switched();
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let draft = responses_draft(&handle);
            let layout = test_profile_layout(&draft);
            let semantic_row = pane_hit_row(&layout, PaneHit::Cancel);
            let offset = 2u16;
            assert!(semantic_row > offset);
            panel
                .right_scroll
                .write()
                .set_offset(Position::new(0, offset));
            *panel.profile_draft.write() = Some(draft);
            let row = pane_click_row(TEST_AREA, &layout, PaneHit::Cancel, offset);
            let result = panel.event(
                left_down(row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &switched,
            );
            tx.send((result, panel.profile_draft.read().is_none()))
                .unwrap();
        }
    });

    let (result, closed) = rx.try_recv().unwrap();
    assert_eq!(result, EventResult::Consumed);
    assert!(closed, "滚动后点击 Cancel 仍须命中同一语义行");
}

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

/// 点击右栏字段行 = 聚焦该字段并执行与键盘相同的行为；其他字段仍是即时保存路径。
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

            // Model 行：点击与 → 相同，进入档位草稿（候选模型前进一格）。
            let model_row = TEST_AREA.y + 3 + FIELD_MODEL as u16;
            let model_result = panel.event(
                left_down(model_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &click_switched,
            );
            let draft = panel
                .profile_draft
                .read()
                .as_ref()
                .map(|draft| (draft.provider_id.clone(), draft.model.clone()));
            let _ = panel.event(key(KeyCode::Esc), TEST_AREA, &click_switched);

            // Provider 行：点击与 → 相同，进入草稿并切到下一个 provider 候选。
            let provider_row = TEST_AREA.y + 3 + FIELD_PROVIDER as u16;
            let provider_result = panel.event(
                left_down(provider_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &click_switched,
            );
            let provider_draft = panel
                .profile_draft
                .read()
                .as_ref()
                .map(|draft| draft.provider_id.clone());
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

            // Effort 行：保持即时保存语义，不进入草稿/editor。
            let effort_row = TEST_AREA.y + 3 + FIELD_EFFORT as u16;
            let effort_result = panel.event(
                left_down(effort_row, right_content_column(TEST_AREA)),
                TEST_AREA,
                &click_switched,
            );
            let effort_focus = (*panel.right_focus.read(), *panel.right_cursor.read());
            let no_editor = panel.window_editor.read().is_none();
            let no_draft = panel.profile_draft.read().is_none();

            tx.send((
                context_result,
                editor,
                context_focus,
                model_result,
                draft,
                provider_result,
                provider_draft,
                max_tokens_result,
                max_tokens_focus,
                effort_result,
                effort_focus,
                no_editor,
                no_draft,
            ))
            .unwrap();
        }
    });

    let (
        context_result,
        editor,
        context_focus,
        model_result,
        draft,
        provider_result,
        provider_draft,
        max_tokens_result,
        max_tokens_focus,
        effort_result,
        focus,
        no_editor,
        no_draft,
    ) = rx.try_recv().unwrap();
    assert_eq!(context_result, EventResult::Consumed);
    assert_eq!(
        editor,
        Some(("sonnet".to_string(), "3000".to_string())),
        "点击 Responses context 行必须进入既有内联 editor"
    );
    assert_eq!(context_focus, (true, FIELD_CONTEXT_1M));
    assert_eq!(model_result, EventResult::Consumed);
    assert_eq!(
        draft,
        Some(("resp".to_string(), "haiku-model".to_string())),
        "点击 Model 行必须进入草稿，且候选与键盘 → 一致"
    );
    assert_eq!(provider_result, EventResult::Consumed);
    assert_eq!(
        provider_draft.as_deref(),
        Some("other"),
        "点击 Provider 行必须进入草稿并切换到下一个候选"
    );
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
    assert!(no_editor && no_draft, "Effort 行不得进入草稿/editor");
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
                panel.profile_draft.read().is_none(),
            ))
            .unwrap();
        }
    });

    let (focus, no_editor, no_draft) = rx.try_recv().unwrap();
    assert_eq!(focus, (false, 0), "空白/边框/滚动条不得移动焦点");
    assert!(no_editor && no_draft, "空白/边框/滚动条不得进入编辑");
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
