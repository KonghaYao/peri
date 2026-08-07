//! ACP protocol state builders.
//!
//! Converts internal agent state into ACP protocol types
//! (modes, models, config options) for `session/new` and `session/set_*` responses.

// [TRAP] build_config_options 必须按优先级顺序返回（mode → model → thinking_effort）
// Session Config Options 覆盖旧的 Session Modes API，顺序错乱会导致 UI 显示异常。

pub use agent_client_protocol_schema::v1::{
    SessionConfigId, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption,
    SessionConfigSelectOptions, SessionConfigValueId, SessionMode, SessionModeId, SessionModeState,
};
use parking_lot::RwLock;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};

use crate::provider::{LlmProvider, PeriConfig, Profiles};

/// Parse a mode ID string into a `PermissionMode`.
pub fn parse_permission_mode(mode_id: &str) -> PermissionMode {
    match mode_id {
        "accept_edit" => PermissionMode::AcceptEdit,
        "auto" => PermissionMode::AutoMode,
        "bypass" => PermissionMode::Bypass,
        _ => PermissionMode::Default,
    }
}

/// Apply a thinking effort level to the active profile (Profile 唯一事实源)。
pub fn apply_profile_effort(peri_config: &RwLock<PeriConfig>, effort: &str) {
    let mut cfg = peri_config.write();
    let alias = cfg.config.active_alias.clone();
    if let Some(profile) = cfg.config.profiles.get_mut(&alias) {
        profile.effort = effort.to_string();
    }
}

/// 兼容 ACP 旧协议消息的薄包装（新代码请用 `apply_profile_effort`）
pub fn apply_thinking_effort(peri_config: &RwLock<PeriConfig>, effort: &str) {
    apply_profile_effort(peri_config, effort);
}

/// Build ACP `SessionModeState` from the current permission mode.
pub fn build_mode_state(pm: &SharedPermissionMode) -> SessionModeState {
    let current = pm.load();
    let current_id = match current {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdit => "accept_edit",
        PermissionMode::AutoMode => "auto",
        PermissionMode::Bypass => "bypass",
    };
    let all_modes = vec![
        SessionMode::new(SessionModeId::new("default"), "Default")
            .description("All sensitive tools require approval"),
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
    _provider: &LlmProvider,
    current_mode: PermissionMode,
) -> Vec<SessionConfigOption> {
    let mut options = Vec::with_capacity(3);

    // ── Mode (category: mode) ──
    let current_mode_id = match current_mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdit => "accept_edit",
        PermissionMode::AutoMode => "auto",
        PermissionMode::Bypass => "bypass",
    };
    let mode_options = vec![
        SessionConfigSelectOption::new(SessionConfigValueId::new("default"), "Default"),
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
    let mut model_options = Vec::new();
    for alias in Profiles::ALL {
        let profile = peri_config.config.profiles.get(alias);
        let model_name = profile
            .and_then(|p| p.model.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| {
                let provider = peri_config.config.providers.iter().find(|prov| {
                    let want = profile.map(|pf| pf.provider.as_str()).unwrap_or("");
                    want.is_empty() || prov.id == want
                });
                provider
                    .and_then(|p| p.models.get_model(alias))
                    .map(str::to_string)
                    .filter(|m| !m.is_empty())
            })
            .unwrap_or_else(|| alias.to_string());
        model_options.push(SessionConfigSelectOption::new(
            SessionConfigValueId::new(alias.to_string()),
            format!("{alias} ({model_name})"),
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
        .profiles
        .get(&peri_config.config.active_alias)
        .map(|p| p.effort.as_str())
        .unwrap_or("xhigh");
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
