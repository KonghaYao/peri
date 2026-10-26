//! Model 上下文窗口 editor 的多行与非法粘贴拒绝回归。

use super::*;

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
