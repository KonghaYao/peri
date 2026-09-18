use super::*;
use ratatui_kit::crossterm::event::KeyEvent;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// [回归测试] 重复点击检查只产生一个待执行请求，退出编辑使结果失效。
#[test]
fn test_connectivity_repeated_click_is_bounded_and_escape_cancels() {
    let mut state = SetupWizardState {
        step: SetupStep::Form,
        form_mode: FormMode::Edit,
        form_focus: FormField::TestConnectivity,
        ..Default::default()
    };
    handle_edit_keys(&mut state, key(KeyCode::Enter));
    let generation = state.connectivity_generation;
    assert!(state.connectivity_in_progress);
    for _ in 0..20 {
        handle_edit_keys(&mut state, key(KeyCode::Enter));
    }
    assert_eq!(state.connectivity_generation, generation);
    handle_edit_keys(&mut state, key(KeyCode::Esc));
    assert!(!state.connectivity_in_progress);
    assert!(state.connectivity_generation > generation);
    assert_eq!(state.form_mode, FormMode::Browse);
}

/// [回归测试] Enter 仅请求保存，完成标记由异步持久化和激活成功后发布。
#[test]
fn test_save_enter_sets_busy_without_claiming_completion() {
    let mut state = SetupWizardState {
        step: SetupStep::Done,
        ..Default::default()
    };
    handle_done_keys(&mut state, key(KeyCode::Enter));
    assert!(state.save_in_progress);
    assert_eq!(state.step, SetupStep::Done);
    let generation = state.connectivity_generation;
    handle_done_keys(&mut state, key(KeyCode::Enter));
    assert!(state.save_in_progress);
    assert_eq!(state.connectivity_generation, generation);
}

/// [回归测试] Delete 和粘贴曾绕过只对字符和退格执行的失效逻辑。
#[test]
fn test_delete_and_paste_cancel_pending_check() {
    let mut state = SetupWizardState {
        step: SetupStep::Form,
        form_mode: FormMode::Edit,
        form_focus: FormField::BaseUrl,
        connectivity_in_progress: true,
        ..Default::default()
    };
    handle_edit_keys(&mut state, key(KeyCode::Delete));
    assert!(!state.connectivity_in_progress);
    state.connectivity_in_progress = true;
    let generation = state.connectivity_generation;
    handle_paste_to_text_input(&mut state, "a");
    assert!(!state.connectivity_in_progress);
    assert!(state.connectivity_generation > generation);
}
