use super::*;
use ratatui_kit::crossterm::event::KeyEvent;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn chatgpt_source_opens_guide_without_touching_provider_draft() {
    let mut state = SetupWizardState {
        step: SetupStep::Choose,
        source: SetupSource::ChatGptSubscription,
        choose_cursor: 2,
        ..Default::default()
    };
    let original = state.providers[0].provider_id.clone();
    handle_choose_keys(&mut state, key(KeyCode::Enter));
    assert_eq!(state.step, SetupStep::ChatGpt);
    assert_eq!(state.chatgpt_cursor, 0);
    assert_eq!(state.providers[0].provider_id, original);
}

#[test]
fn chatgpt_guide_keyboard_cancel_returns_to_source() {
    let mut state = SetupWizardState {
        step: SetupStep::ChatGpt,
        ..Default::default()
    };
    handle_chatgpt_keys(&mut state, key(KeyCode::Tab));
    assert_eq!(state.chatgpt_cursor, 1);
    handle_chatgpt_keys(&mut state, key(KeyCode::Enter));
    assert_eq!(state.step, SetupStep::Choose);
    state.step = SetupStep::ChatGpt;
    handle_chatgpt_keys(&mut state, key(KeyCode::Esc));
    assert_eq!(state.step, SetupStep::Choose);
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
    for _ in 0..3 {
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

/// [回归测试] 从 Choose 显式选择 CustomApi 不得重置已有 provider 草稿。
#[test]
fn test_choose_custom_api_keeps_existing_providers() {
    let mut existing = MigratedProvider::new(ProviderType::OpenAiCompatible);
    existing.provider_id = "custom".into();
    existing.api_key = "custom-key".into();
    existing.base_url = "https://custom.example/v1".into();
    existing.aliases = [
        "fable-keep".into(),
        "opus-keep".into(),
        "sonnet-keep".into(),
        "haiku-keep".into(),
    ];
    let mut state = SetupWizardState {
        step: SetupStep::Choose,
        source: SetupSource::CustomApi,
        providers: vec![existing],
        ..Default::default()
    };
    handle_choose_keys(&mut state, key(KeyCode::Enter));
    assert_eq!(state.step, SetupStep::Form);
    assert_eq!(state.providers.len(), 1);
    assert_eq!(state.providers[0].provider_id, "custom");
    assert_eq!(state.providers[0].api_key, "custom-key");
    assert_eq!(state.providers[0].base_url, "https://custom.example/v1");
    assert_eq!(state.providers[0].aliases[0], "fable-keep");
    assert_eq!(state.active_provider, 0);

    // 首次配置没有 provider 时仍创建默认草稿。
    let mut fresh = SetupWizardState {
        step: SetupStep::Choose,
        source: SetupSource::CustomApi,
        providers: Vec::new(),
        ..Default::default()
    };
    handle_choose_keys(&mut fresh, key(KeyCode::Enter));
    assert_eq!(fresh.step, SetupStep::Form);
    assert_eq!(fresh.providers.len(), 1);
    assert_eq!(fresh.providers[0].provider_id, "anthropic");
    assert!(fresh.providers[0].api_key.is_empty());
}

/// [回归测试] 已有配置直接进入 Form，Esc 仍回到包含 ChatGPT 订阅入口的 Choose。
#[test]
fn test_reopened_form_escape_returns_to_choose_with_chatgpt_entry() {
    let mut state = SetupWizardState {
        step: SetupStep::Form,
        form_mode: FormMode::Browse,
        ..Default::default()
    };
    handle_form_keys(&mut state, key(KeyCode::Esc));
    assert_eq!(state.step, SetupStep::Choose);
    assert!(SetupSource::ALL.contains(&SetupSource::ChatGptSubscription));
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
