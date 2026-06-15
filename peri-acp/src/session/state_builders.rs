//! ACP protocol state builders.
//!
//! Converts internal agent state into ACP protocol types
//! (modes, models, config options) for `session/new` and `session/set_*` responses.

pub use agent_client_protocol_schema::{
    SessionConfigId, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption,
    SessionConfigSelectOptions, SessionConfigValueId, SessionMode, SessionModeId, SessionModeState,
};
use parking_lot::RwLock;
use peri_middlewares::prelude::{PermissionMode, SharedPermissionMode};

use crate::provider::{LlmProvider, PeriConfig, ThinkingConfig};

/// Parse a mode ID string into a `PermissionMode`.
pub fn parse_permission_mode(mode_id: &str) -> PermissionMode {
    match mode_id {
        "dont_ask" => PermissionMode::DontAsk,
        "accept_edit" => PermissionMode::AcceptEdit,
        "auto" => PermissionMode::AutoMode,
        "bypass" => PermissionMode::Bypass,
        _ => PermissionMode::Default,
    }
}

/// Apply a thinking effort level to `PeriConfig` (writes through `RwLock`).
pub fn apply_thinking_effort(peri_config: &RwLock<PeriConfig>, effort: &str) {
    let mut cfg = peri_config.write();
    let thinking = cfg.config.thinking.get_or_insert_with(|| ThinkingConfig {
        enabled: true,
        budget_tokens: 8000,
        effort: "medium".to_string(),
        max_tokens: 32000,
    });
    thinking.enabled = true;
    thinking.effort = effort.to_string();
}

/// Build ACP `SessionModeState` from the current permission mode.
pub fn build_mode_state(pm: &SharedPermissionMode) -> SessionModeState {
    let current = pm.load();
    let current_id = match current {
        PermissionMode::Default => "default",
        PermissionMode::DontAsk => "dont_ask",
        PermissionMode::AcceptEdit => "accept_edit",
        PermissionMode::AutoMode => "auto",
        PermissionMode::Bypass => "bypass",
    };
    let all_modes = vec![
        SessionMode::new(SessionModeId::new("default"), "Default")
            .description("All sensitive tools require approval"),
        SessionMode::new(SessionModeId::new("dont_ask"), "Don't Ask")
            .description("Default deny all bash"),
        SessionMode::new(SessionModeId::new("accept_edit"), "Accept Edit")
            .description("Allow filesystem edits"),
        SessionMode::new(SessionModeId::new("auto"), "Auto Mode")
            .description("LLM decides approval"),
        SessionMode::new(SessionModeId::new("bypass"), "Bypass").description("Allow everything"),
    ];
    SessionModeState::new(SessionModeId::new(current_id), all_modes)
}

/// Build ACP `SessionConfigOption` list from config.
///
/// Per ACP spec, config options supersede the older Session Modes API.
/// Returns mode, model, and thinking_effort in priority order (higher priority first).
pub fn build_config_options(
    peri_config: &PeriConfig,
    provider: &LlmProvider,
    current_mode: PermissionMode,
) -> Vec<SessionConfigOption> {
    let mut options = Vec::with_capacity(3);

    // ── Mode (category: mode) ──
    let current_mode_id = match current_mode {
        PermissionMode::Default => "default",
        PermissionMode::DontAsk => "dont_ask",
        PermissionMode::AcceptEdit => "accept_edit",
        PermissionMode::AutoMode => "auto",
        PermissionMode::Bypass => "bypass",
    };
    let mode_options = vec![
        SessionConfigSelectOption::new(SessionConfigValueId::new("default"), "Default"),
        SessionConfigSelectOption::new(SessionConfigValueId::new("dont_ask"), "Don't Ask"),
        SessionConfigSelectOption::new(SessionConfigValueId::new("accept_edit"), "Accept Edit"),
        SessionConfigSelectOption::new(SessionConfigValueId::new("auto"), "Auto Mode"),
        SessionConfigSelectOption::new(SessionConfigValueId::new("bypass"), "Bypass"),
    ];
    options.push(
        SessionConfigOption::select(
            SessionConfigId::new("mode"),
            "Session Mode",
            SessionConfigValueId::new(current_mode_id),
            SessionConfigSelectOptions::Ungrouped(mode_options),
        )
        .category(SessionConfigOptionCategory::Mode),
    );

    // ── Model (category: model) ──
    let active_alias = peri_config.config.active_alias.clone();
    let active_provider = peri_config.config.providers.iter().find(|prov| {
        prov.id == peri_config.config.active_provider_id
            || peri_config.config.active_provider_id.is_empty()
    });
    let mut model_options = Vec::new();
    if let Some(prov) = active_provider {
        for alias in ["opus", "sonnet", "haiku"] {
            if let Some(model_name) = prov.models.get_model(alias) {
                if !model_name.is_empty() {
                    model_options.push(SessionConfigSelectOption::new(
                        SessionConfigValueId::new(alias.to_string()),
                        format!("{} ({})", alias, model_name),
                    ));
                }
            }
        }
    }
    if model_options.is_empty() {
        model_options.push(SessionConfigSelectOption::new(
            SessionConfigValueId::new("current".to_string()),
            provider.model_name().to_string(),
        ));
    }
    options.push(
        SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            SessionConfigValueId::new(active_alias),
            SessionConfigSelectOptions::Ungrouped(model_options),
        )
        .category(SessionConfigOptionCategory::Model),
    );

    // ── Thinking effort (category: thought_level) ──
    let effort = peri_config
        .config
        .thinking
        .as_ref()
        .map(|t| t.effort.as_str())
        .unwrap_or("medium");
    let thinking_options = vec![
        SessionConfigSelectOption::new(SessionConfigValueId::new("low"), "Low".to_string()),
        SessionConfigSelectOption::new(SessionConfigValueId::new("medium"), "Medium".to_string()),
        SessionConfigSelectOption::new(SessionConfigValueId::new("high"), "High".to_string()),
        SessionConfigSelectOption::new(SessionConfigValueId::new("xhigh"), "XHigh".to_string()),
        SessionConfigSelectOption::new(SessionConfigValueId::new("max"), "Max".to_string()),
    ];
    options.push(
        SessionConfigOption::select(
            SessionConfigId::new("thinking_effort"),
            "Thinking Effort",
            SessionConfigValueId::new(effort),
            SessionConfigSelectOptions::Ungrouped(thinking_options),
        )
        .category(SessionConfigOptionCategory::ThoughtLevel),
    );

    options
}
