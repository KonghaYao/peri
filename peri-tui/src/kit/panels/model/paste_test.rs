//! Model editor/草稿的多行与非法粘贴拒绝回归。

use super::*;

// ── 设计 §4.5：Model editor 的粘贴拒绝 ─────────────────────────────────────

/// window 数值 editor：多行粘贴整段拒绝、文本不变、错误可见且阻断保存；
/// 退格清除拒绝状态后可继续编辑与提交。
#[test]
#[serial]
fn window_editor_paste_multiline_is_rejected_visibly() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let before = handle.read().clone();
    let switched = switched();
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let editor = WindowEditState::enter(&handle.read(), "sonnet", None)
                .expect("Responses 档位必须有内联 editor");
            assert_eq!(editor.text, "3000");
            *panel.window_editor.write() = Some(editor);

            let paste = panel.event(Event::Paste("12\n34".into()), TEST_AREA, &switched);
            let rejected = panel.window_editor.read().as_ref().map(|editor| {
                (
                    editor.text.clone(),
                    editor.error.clone(),
                    editor.input_blocked,
                )
            });

            let enter = panel.event(key(KeyCode::Enter), TEST_AREA, &switched);
            let blocked = (
                panel.window_editor.read().is_some(),
                panel
                    .window_editor
                    .read()
                    .as_ref()
                    .map(|editor| (editor.error.clone(), editor.input_blocked)),
            );

            let _ = panel.event(key(KeyCode::Backspace), TEST_AREA, &switched);
            let corrected = panel.window_editor.read().as_ref().map(|editor| {
                (
                    editor.text.clone(),
                    editor.error.clone(),
                    editor.input_blocked,
                )
            });
            let accepted = panel.event(Event::Paste("56".into()), TEST_AREA, &switched);
            let after_valid_paste = panel.window_editor.read().as_ref().map(|editor| {
                (
                    editor.text.clone(),
                    editor.error.clone(),
                    editor.input_blocked,
                )
            });

            tx.send((
                paste,
                rejected,
                enter,
                blocked,
                corrected,
                accepted,
                after_valid_paste,
            ))
            .unwrap();
        }
    });

    let (paste, rejected, enter, blocked, corrected, accepted, after_valid_paste) =
        rx.try_recv().unwrap();
    assert_eq!(paste, EventResult::Consumed);
    let invalid = Some(i18n::tr("model-panel-window-invalid"));
    assert_eq!(
        rejected,
        Some(("3000".to_string(), invalid.clone(), true)),
        "多行粘贴不得写入，且必须留下可见错误与阻断标记"
    );
    assert_eq!(enter, EventResult::Consumed);
    assert_eq!(
        blocked,
        (true, Some((invalid, true))),
        "被拒绝的输入未修正前不得提交，editor 保留"
    );
    assert_eq!(
        corrected,
        Some(("300".to_string(), None, false)),
        "修正后错误生命周期结束"
    );
    assert_eq!(accepted, EventResult::Consumed);
    assert_eq!(
        after_valid_paste,
        Some(("30056".to_string(), None, false)),
        "纯数字粘贴可正常写入"
    );
    assert!(
        guard.handle.read().config == before.config,
        "粘贴拒绝与 editor 提交都不得改动未落盘的配置"
    );
}

/// model 文本 editor：多行粘贴整段拒绝、文本不变、错误可见且阻断保存；
/// 修正后可继续（真实落盘由 commit 用例覆盖）。
#[test]
#[serial]
fn draft_model_paste_multiline_is_rejected_visibly() {
    let guard = ModelPanelGuard::install();
    guard.set_config(responses_config());
    let handle = Arc::clone(&guard.handle);
    let switched = switched();
    let (tx, rx) = std::sync::mpsc::channel();

    mount_panel_frames(TEST_AREA, 1, move |frame, panel| {
        if frame == 0 {
            let mut draft = responses_draft(&handle);
            draft.focus = DraftFocus::Model;
            draft.begin_model_edit();
            assert_eq!(draft.model, "next-model");
            *panel.profile_draft.write() = Some(draft);

            let paste = panel.event(Event::Paste("evil\nmodel".into()), TEST_AREA, &switched);
            let rejected = panel.profile_draft.read().as_ref().map(|draft| {
                (
                    draft.model.clone(),
                    draft.error.clone(),
                    draft.input_blocked,
                    draft.model_editing,
                )
            });

            // Enter 确认并落到 Save；再次 Enter 被子拒绝输入阻断。
            let _ = panel.event(key(KeyCode::Enter), TEST_AREA, &switched);
            let enter = panel.event(key(KeyCode::Enter), TEST_AREA, &switched);
            let blocked = (
                panel.profile_draft.read().is_some(),
                panel
                    .profile_draft
                    .read()
                    .as_ref()
                    .map(|draft| (draft.error.clone(), draft.input_blocked)),
            );

            // 回到 Model 行重新进入文本编辑即清除字段错误，再输入一个字符修正。
            let _ = panel.event(key(KeyCode::Up), TEST_AREA, &switched);
            let _ = panel.event(key(KeyCode::Up), TEST_AREA, &switched);
            let _ = panel.event(key(KeyCode::Enter), TEST_AREA, &switched);
            let _ = panel.event(key(KeyCode::Char('x')), TEST_AREA, &switched);
            let corrected = panel.profile_draft.read().as_ref().map(|draft| {
                (
                    draft.model.clone(),
                    draft.error.clone(),
                    draft.input_blocked,
                )
            });

            // 再次确认并尝试提交：先补齐新目标窗口；无 authority 时保存失败，
            // 但已不再被粘贴错误阻断。
            let _ = panel.event(key(KeyCode::Enter), TEST_AREA, &switched);
            if let Some(draft) = panel.profile_draft.write().as_mut() {
                draft.text = "128000".into();
                draft.cursor = 6;
            }
            let _ = panel.event(key(KeyCode::Enter), TEST_AREA, &switched);
            let attempted = panel
                .profile_draft
                .read()
                .as_ref()
                .and_then(|draft| draft.error.clone());

            // 草稿窗口字段同样拒绝多行粘贴，纯数字粘贴可接受。
            if let Some(draft) = panel.profile_draft.write().as_mut() {
                draft.focus = DraftFocus::Window;
                draft.text.clear();
                draft.cursor = 0;
            }
            let _ = panel.event(Event::Paste("12\n34".into()), TEST_AREA, &switched);
            let window_rejected = panel
                .profile_draft
                .read()
                .as_ref()
                .map(|draft| (draft.text.clone(), draft.error.clone(), draft.input_blocked));
            let _ = panel.event(Event::Paste("128000".into()), TEST_AREA, &switched);
            let window_accepted = panel
                .profile_draft
                .read()
                .as_ref()
                .map(|draft| (draft.text.clone(), draft.error.clone(), draft.input_blocked));

            tx.send((
                paste,
                rejected,
                enter,
                blocked,
                corrected,
                attempted,
                window_rejected,
                window_accepted,
            ))
            .unwrap();
        }
    });

    let (paste, rejected, enter, blocked, corrected, attempted, window_rejected, window_accepted) =
        rx.try_recv().unwrap();
    assert_eq!(paste, EventResult::Consumed);
    let single_line = Some(i18n::tr("model-panel-model-single-line"));
    assert_eq!(
        rejected,
        Some(("next-model".to_string(), single_line.clone(), true, true)),
        "多行粘贴不得写入 model 文本，且必须留下可见错误"
    );
    assert_eq!(enter, EventResult::Consumed);
    assert_eq!(
        blocked,
        (true, Some((single_line, true))),
        "被拒绝的输入未修正前不得提交，草稿保留"
    );
    assert_eq!(
        corrected,
        Some(("next-modelx".to_string(), None, false)),
        "重新进入编辑并输入字符后错误生命周期结束"
    );
    assert_eq!(
        attempted,
        Some(i18n::tr("model-panel-save-failed-error")),
        "修正后提交会走到保存边界（无 authority 时失败），不再被粘贴错误阻断"
    );
    assert_eq!(
        window_rejected,
        Some((
            String::new(),
            Some(i18n::tr("model-panel-window-invalid")),
            true
        )),
        "草稿窗口字段同样整段拒绝多行粘贴"
    );
    assert_eq!(
        window_accepted,
        Some(("128000".to_string(), None, false)),
        "纯数字粘贴可正常写入窗口字段"
    );
}
