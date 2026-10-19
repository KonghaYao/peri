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

/// [回归测试] 选择「供应商预设」只进入 Preset 步骤，不提前重建 provider 列表。
#[test]
fn test_choose_preset_source_enters_preset_step_without_touching_providers() {
    let mut state = SetupWizardState {
        step: SetupStep::Choose,
        submit_error: Some("stale".into()),
        ..Default::default()
    };
    let original_id = state.providers[0].provider_id.clone();
    for _ in 0..2 {
        handle_choose_keys(&mut state, key(KeyCode::Down));
    }
    assert_eq!(state.source, SetupSource::ProviderPreset);
    handle_choose_keys(&mut state, key(KeyCode::Enter));
    assert_eq!(state.step, SetupStep::Preset);
    assert_eq!(state.preset_cursor, 0);
    assert!(state.submit_error.is_none());
    assert_eq!(state.providers.len(), 1);
    assert_eq!(state.providers[0].provider_id, original_id);
}

/// [回归测试] Preset 光标在首尾环绕，移动时清除过期错误。
#[test]
fn test_preset_cursor_wraps_and_clears_error() {
    let mut state = SetupWizardState {
        step: SetupStep::Preset,
        submit_error: Some("stale".into()),
        ..Default::default()
    };
    handle_preset_keys(&mut state, key(KeyCode::Up));
    assert_eq!(
        state.preset_cursor,
        PROVIDER_PRESETS.len() - 1,
        "首项向上环绕"
    );
    assert!(state.submit_error.is_none());
    handle_preset_keys(&mut state, key(KeyCode::Down));
    assert_eq!(state.preset_cursor, 0, "末项向下环绕");
}

/// [回归测试] 确认预设后预填 provider，光标落在 API Key 字段，待执行的检查失效。
#[test]
fn test_preset_enter_prefills_provider_and_focuses_api_key() {
    let preset = &PROVIDER_PRESETS[1];
    let mut state = SetupWizardState {
        step: SetupStep::Preset,
        preset_cursor: 1,
        connectivity_in_progress: true,
        ..Default::default()
    };
    handle_preset_keys(&mut state, key(KeyCode::Enter));
    assert_eq!(state.step, SetupStep::Form);
    assert_eq!(state.form_mode, FormMode::Edit);
    assert_eq!(state.form_focus, FormField::ApiKey);
    assert_eq!(state.active_provider, 0);
    assert_eq!(state.providers.len(), 1);
    let mp = &state.providers[0];
    assert_eq!(mp.provider_type, preset.provider_type);
    assert_eq!(mp.provider_id, preset.provider_id);
    assert_eq!(mp.base_url, preset.base_url);
    assert_eq!(mp.aliases, preset.models.map(String::from));
    assert_eq!(state.edit_cursor_pos, mp.api_key.chars().count());
    assert!(!state.connectivity_in_progress);
}

/// [回归测试] 确认预设后浏览光标复位到首项，不沿用旧列表位置。
#[test]
fn test_preset_enter_resets_browse_cursor() {
    let mut state = SetupWizardState {
        step: SetupStep::Preset,
        browse_cursor: 3,
        ..Default::default()
    };
    handle_preset_keys(&mut state, key(KeyCode::Enter));
    assert_eq!(state.step, SetupStep::Form);
    assert_eq!(state.browse_cursor, 0, "重建 provider 列表后光标复位");
}

#[test]
fn test_preset_escape_returns_to_choose() {
    let mut state = SetupWizardState {
        step: SetupStep::Preset,
        submit_error: Some("stale".into()),
        ..Default::default()
    };
    handle_preset_keys(&mut state, key(KeyCode::Esc));
    assert_eq!(state.step, SetupStep::Choose);
    assert!(state.submit_error.is_none());
}
