use tui_textarea::Input;

use super::{
    FormField, FormMode, LANGUAGE_OPTIONS, MigratedProvider, ProviderType, SetupSource, SetupStep,
    SetupWizardPanel, test_connectivity,
};
use crate::app::FieldTextarea;

/// 检测配置是否需要 Setup 向导
pub fn needs_setup(config: &crate::config::AppConfig) -> bool {
    if config.providers.is_empty() {
        // 没有 providers 条目时，检查是否可通过环境变量提供 provider
        // 避免已有 env 配置但 settings.json 格式不标准时阻塞用户
        return crate::app::agent::LlmProvider::from_env().is_none();
    }
    for provider in &config.providers {
        if provider.id.trim().is_empty() {
            return true;
        }
        if provider.api_key.is_empty() {
            let key_env = match provider.provider_type.as_str() {
                "anthropic" => "ANTHROPIC_API_KEY",
                _ => "OPENAI_API_KEY",
            };
            if std::env::var(key_env).unwrap_or_default().is_empty() {
                return true;
            }
        }
    }
    false
}

/// setup_wizard 按键处理的返回动作
pub enum SetupWizardAction {
    Redraw,
    SaveAndClose,
    Skip,
    SetLanguage(String),
}

/// Setup 向导按键分发
pub fn handle_setup_wizard_key(
    wizard: &mut SetupWizardPanel,
    input: Input,
) -> Option<SetupWizardAction> {
    match wizard.step {
        SetupStep::Choose => handle_step_choose(wizard, input),
        SetupStep::Language => handle_step_language(wizard, input),
        SetupStep::Form => handle_step_form(wizard, input),
        SetupStep::Done => handle_step_done(wizard, input),
    }
}

fn handle_step_choose(wizard: &mut SetupWizardPanel, input: Input) -> Option<SetupWizardAction> {
    use tui_textarea::Key;
    debug_assert!(
        !SetupSource::ALL.is_empty(),
        "SetupSource::ALL must not be empty"
    );
    match input {
        Input { key: Key::Up, .. } => {
            wizard.choose_cursor =
                (wizard.choose_cursor + SetupSource::ALL.len() - 1) % SetupSource::ALL.len();
            wizard.source = SetupSource::ALL[wizard.choose_cursor];
            Some(SetupWizardAction::Redraw)
        }
        Input { key: Key::Down, .. } => {
            wizard.choose_cursor = (wizard.choose_cursor + 1) % SetupSource::ALL.len();
            wizard.source = SetupSource::ALL[wizard.choose_cursor];
            Some(SetupWizardAction::Redraw)
        }
        Input {
            key: Key::Enter, ..
        }
        | Input {
            key: Key::Char(' '),
            ..
        } => {
            if wizard.source == SetupSource::MigrateClaudeCode {
                if !wizard.migrate_from_claude_code() {
                    wizard.source = SetupSource::CustomApi;
                    wizard.choose_cursor = 0;
                    return Some(SetupWizardAction::Redraw);
                }
            } else {
                wizard.providers = vec![MigratedProvider::new(ProviderType::Anthropic)];
                wizard.active_provider = 0;
            }
            wizard.step = SetupStep::Form;
            wizard.form_mode = FormMode::Browse;
            wizard.browse_cursor = 0;
            wizard.form_focus = FormField::ProviderType;
            Some(SetupWizardAction::Redraw)
        }
        Input { key: Key::Esc, .. } => {
            wizard.step = SetupStep::Language;
            Some(SetupWizardAction::Redraw)
        }
        _ => None,
    }
}

fn handle_step_language(wizard: &mut SetupWizardPanel, input: Input) -> Option<SetupWizardAction> {
    use tui_textarea::Key;
    debug_assert!(
        !LANGUAGE_OPTIONS.is_empty(),
        "LANGUAGE_OPTIONS must not be empty"
    );
    match input {
        Input { key: Key::Up, .. } => {
            wizard.language_cursor =
                (wizard.language_cursor + LANGUAGE_OPTIONS.len() - 1) % LANGUAGE_OPTIONS.len();
            Some(SetupWizardAction::Redraw)
        }
        Input { key: Key::Down, .. } => {
            wizard.language_cursor = (wizard.language_cursor + 1) % LANGUAGE_OPTIONS.len();
            Some(SetupWizardAction::Redraw)
        }
        Input {
            key: Key::Enter, ..
        }
        | Input {
            key: Key::Char(' '),
            ..
        } => {
            let lang = LANGUAGE_OPTIONS[wizard.language_cursor].0.to_string();
            wizard.language = lang.clone();
            wizard.step = SetupStep::Choose;
            wizard.choose_cursor = 0;
            Some(SetupWizardAction::SetLanguage(lang))
        }
        Input { key: Key::Esc, .. } => Some(SetupWizardAction::Skip),
        _ => None,
    }
}

fn handle_step_form(wizard: &mut SetupWizardPanel, input: Input) -> Option<SetupWizardAction> {
    match wizard.form_mode {
        FormMode::Browse => handle_browse(wizard, input),
        FormMode::Edit => handle_edit(wizard, input),
    }
}

fn handle_browse(wizard: &mut SetupWizardPanel, input: Input) -> Option<SetupWizardAction> {
    use tui_textarea::Key;
    let max_pos = wizard.providers.len();
    match input {
        Input { key: Key::Up, .. } => {
            wizard.submit_error = None;
            if wizard.browse_cursor > 0 {
                wizard.browse_cursor -= 1;
            }
            Some(SetupWizardAction::Redraw)
        }
        Input { key: Key::Down, .. } => {
            wizard.submit_error = None;
            if wizard.browse_cursor < max_pos {
                wizard.browse_cursor += 1;
            }
            Some(SetupWizardAction::Redraw)
        }
        Input {
            key: Key::Char(' '),
            ..
        } => {
            wizard.submit_error = None;
            if wizard.browse_cursor < wizard.providers.len() {
                let mp = &mut wizard.providers[wizard.browse_cursor];
                mp.selected = !mp.selected;
                Some(SetupWizardAction::Redraw)
            } else {
                None
            }
        }
        Input {
            key: Key::Enter, ..
        } => {
            if wizard.browse_cursor < wizard.providers.len() {
                wizard.submit_error = None;
                wizard.active_provider = wizard.browse_cursor;
                wizard.form_mode = FormMode::Edit;
                wizard.form_focus = FormField::ProviderType;
                Some(SetupWizardAction::Redraw)
            } else {
                let has_valid = wizard
                    .providers
                    .iter()
                    .any(|p| p.selected && p.is_complete());
                if has_valid {
                    wizard.submit_error = None;
                    wizard.step = SetupStep::Done;
                } else {
                    wizard.submit_error = Some(
                        "No provider selected or incomplete. Select at least one provider with all fields filled."
                            .into(),
                    );
                }
                Some(SetupWizardAction::Redraw)
            }
        }
        Input { key: Key::Esc, .. } => {
            wizard.submit_error = None;
            wizard.step = SetupStep::Choose;
            Some(SetupWizardAction::Redraw)
        }
        _ => None,
    }
}

fn handle_edit(wizard: &mut SetupWizardPanel, input: Input) -> Option<SetupWizardAction> {
    use tui_textarea::Key;
    if wizard.form_focus == FormField::BaseUrl {
        wizard.connectivity_result = None;
    }
    match input {
        Input { key: Key::Up, .. } => {
            wizard.form_focus = wizard.form_focus.prev();
            Some(SetupWizardAction::Redraw)
        }
        Input { key: Key::Down, .. } => {
            wizard.form_focus = wizard.form_focus.next();
            Some(SetupWizardAction::Redraw)
        }
        Input {
            key: Key::Left,
            ctrl: false,
            ..
        }
        | Input {
            key: Key::Right,
            ctrl: false,
            ..
        } => {
            if wizard.form_focus == FormField::ProviderType {
                let mp = &mut wizard.providers[wizard.active_provider];
                mp.provider_type.cycle();
                Some(SetupWizardAction::Redraw)
            } else if let Some(field) = get_active_field(wizard) {
                if field.input(input) {
                    Some(SetupWizardAction::Redraw)
                } else {
                    None
                }
            } else {
                None
            }
        }
        Input {
            key: Key::Char(' '),
            ..
        } => {
            if wizard.form_focus == FormField::ProviderType {
                let mp = &mut wizard.providers[wizard.active_provider];
                mp.provider_type.cycle();
                Some(SetupWizardAction::Redraw)
            } else if let Some(field) = get_active_field(wizard) {
                if field.input(input) {
                    Some(SetupWizardAction::Redraw)
                } else {
                    None
                }
            } else {
                None
            }
        }
        Input {
            key: Key::Enter, ..
        } => {
            if wizard.form_focus == FormField::TestConnectivity {
                let mp = &wizard.providers[wizard.active_provider];
                wizard.connectivity_result = Some(test_connectivity(&mp.field_base_url.value()));
                Some(SetupWizardAction::Redraw)
            } else if wizard.form_focus == FormField::Confirm {
                let mp = &wizard.providers[wizard.active_provider];
                if !mp.field_provider_id.value().trim().is_empty()
                    && !mp.field_api_key.value().trim().is_empty()
                    && mp
                        .aliases
                        .iter()
                        .all(|a| !a.field_model_id.value().trim().is_empty())
                {
                    wizard.form_mode = FormMode::Browse;
                    Some(SetupWizardAction::Redraw)
                } else {
                    Some(SetupWizardAction::Redraw)
                }
            } else {
                None
            }
        }
        Input { key: Key::Esc, .. } => {
            wizard.form_mode = FormMode::Browse;
            Some(SetupWizardAction::Redraw)
        }
        _ => {
            if let Some(field) = get_active_field(wizard) {
                if field.input(input) {
                    Some(SetupWizardAction::Redraw)
                } else {
                    None
                }
            } else {
                None
            }
        }
    }
}

fn get_active_field(wizard: &mut SetupWizardPanel) -> Option<&mut FieldTextarea> {
    if !wizard.form_focus.is_text_input() {
        return None;
    }
    let mp = &mut wizard.providers[wizard.active_provider];
    provider_field_buf(mp, wizard.form_focus)
}

pub(crate) fn provider_field_buf(
    mp: &mut MigratedProvider,
    field: FormField,
) -> Option<&mut FieldTextarea> {
    match field {
        FormField::ProviderId => Some(&mut mp.field_provider_id),
        FormField::BaseUrl => Some(&mut mp.field_base_url),
        FormField::ApiKey => Some(&mut mp.field_api_key),
        FormField::OpusModel => Some(&mut mp.aliases[0].field_model_id),
        FormField::SonnetModel => Some(&mut mp.aliases[1].field_model_id),
        FormField::HaikuModel => Some(&mut mp.aliases[2].field_model_id),
        _ => None,
    }
}

fn handle_step_done(wizard: &mut SetupWizardPanel, input: Input) -> Option<SetupWizardAction> {
    use tui_textarea::Key;
    match input {
        Input {
            key: Key::Enter, ..
        } => Some(SetupWizardAction::SaveAndClose),
        Input { key: Key::Esc, .. } => {
            wizard.submit_error = None;
            wizard.step = SetupStep::Form;
            wizard.form_mode = FormMode::Browse;
            Some(SetupWizardAction::Redraw)
        }
        _ => None,
    }
}

/// 从 wizard 数据构建 PeriConfig（纯数据转换，无磁盘 I/O）
pub fn build_wizard_config(wizard: &SetupWizardPanel) -> crate::config::PeriConfig {
    let mut cfg = crate::config::PeriConfig::default();
    let mut first_id = String::new();

    for mp in &wizard.providers {
        if !mp.selected {
            continue;
        }
        if mp.field_provider_id.value().trim().is_empty()
            || mp.field_api_key.value().trim().is_empty()
        {
            continue;
        }
        let provider = crate::config::ProviderConfig {
            id: mp.field_provider_id.value(),
            provider_type: mp.provider_type.type_str().to_string(),
            api_key: mp.field_api_key.value(),
            base_url: mp.field_base_url.value(),
            models: crate::config::ProviderModels {
                opus: mp.aliases[0].field_model_id.value(),
                sonnet: mp.aliases[1].field_model_id.value(),
                haiku: mp.aliases[2].field_model_id.value(),
            },
            ..Default::default()
        };
        if first_id.is_empty() {
            first_id = provider.id.clone();
        }
        cfg.config.providers.push(provider);
    }

    if !first_id.is_empty() {
        cfg.config.active_alias = "opus".to_string();
        cfg.config.active_provider_id = first_id;
    }

    cfg.config.language = Some(wizard.language.clone());
    cfg
}

/// 将 setup wizard 结果写入指定路径
pub fn save_setup_to(
    wizard: &SetupWizardPanel,
    path: &std::path::Path,
) -> anyhow::Result<crate::config::PeriConfig> {
    let cfg = build_wizard_config(wizard);
    crate::config::save_to(&cfg, path)?;
    Ok(cfg)
}

/// 将 setup wizard 结果合并到已有配置并保存
pub fn save_setup(wizard: &SetupWizardPanel) -> anyhow::Result<crate::config::PeriConfig> {
    let mut merged = crate::config::load().unwrap_or_else(|_| crate::config::PeriConfig::default());

    let wizard_cfg = build_wizard_config(wizard);

    for new_provider in &wizard_cfg.config.providers {
        if !merged
            .config
            .providers
            .iter()
            .any(|p| p.id == new_provider.id)
        {
            merged.config.providers.push(new_provider.clone());
        }
    }

    if !wizard_cfg.config.active_provider_id.is_empty() {
        merged.config.active_alias = wizard_cfg.config.active_alias;
        merged.config.active_provider_id = wizard_cfg.config.active_provider_id;
    }

    if let Some(lang) = wizard_cfg.config.language {
        merged.config.language = Some(lang);
    }

    crate::config::save(&merged)?;
    Ok(merged)
}
