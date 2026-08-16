//! Setup Wizard —— 首次启动配置向导。
//!
//! 完整交互式向导流程：
//! 1. Language  → 选择界面语言（en / zh-CN）
//! 2. Choose    → 选择配置来源（手动输入 / 从 Claude Code 迁移）
//! 3. Form      → 多 Provider 列表浏览 + 编辑详情
//! 4. Done      → 确认并保存
//!
//! 状态通过 `atoms::SETUP_WIZARD` 管理，组件通过 `WIZARD_ACTIVE` 控制显隐。

use serde::{Deserialize, Serialize};

// ── 步骤枚举 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetupStep {
    Language,
    Choose,
    Form,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetupSource {
    CustomApi,
    MigrateClaudeCode,
    PeriFreeService,
}

impl SetupSource {
    pub const ALL: [Self; 3] = [
        Self::CustomApi,
        Self::MigrateClaudeCode,
        Self::PeriFreeService,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::CustomApi => "setup-source-custom-api",
            Self::MigrateClaudeCode => "setup-source-migrate",
            Self::PeriFreeService => "setup-source-peri-free",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::CustomApi => "setup-source-custom-desc",
            Self::MigrateClaudeCode => "setup-source-migrate-desc",
            Self::PeriFreeService => "setup-source-peri-free-desc",
        }
    }
}

/// Peri Code 免费服务快速配置（与 tavily-search/.peri/settings.json 中的
/// provider 配置等同：公共网关、固定档位模型）。
pub const PERI_FREE_BASE_URL: &str = "https://tavily.claude-code-best.win/peri-model/v1";
pub const PERI_FREE_API_KEY: &str = "public";
pub const PERI_FREE_PROVIDER_ID: &str = "peri";
/// 顺序：fable → opus → sonnet → haiku
pub const PERI_FREE_MODEL_IDS: [&str; 4] = ["peri-fable", "peri-opus", "peri-sonnet", "peri-haiku"];

/// 构造 Peri Code 免费服务的 Provider 快速配置
pub fn peri_free_provider() -> MigratedProvider {
    let mut mp = MigratedProvider::new(ProviderType::OpenAiCompatible);
    mp.provider_id = PERI_FREE_PROVIDER_ID.to_string();
    mp.base_url = PERI_FREE_BASE_URL.to_string();
    mp.api_key = PERI_FREE_API_KEY.to_string();
    mp.aliases = PERI_FREE_MODEL_IDS.map(|s| s.to_string());
    mp
}

// ── 语言选项 ──────────────────────────────────────────────────────────────────

pub const LANGUAGE_OPTIONS: [(&str, &str); 2] = [("en", "English"), ("zh-CN", "中文")];

// ── Provider 类型 ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderType {
    Anthropic,
    OpenAiCompatible,
}

impl ProviderType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Anthropic => "setup-provider-anthropic",
            Self::OpenAiCompatible => "setup-provider-openai",
        }
    }

    pub fn type_str(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCompatible => "openai",
        }
    }

    pub fn cycle(&mut self) {
        *self = match self {
            Self::Anthropic => Self::OpenAiCompatible,
            Self::OpenAiCompatible => Self::Anthropic,
        };
    }

    pub fn default_provider_id(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCompatible => "openai",
        }
    }

    pub fn default_base_url(&self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.anthropic.com",
            Self::OpenAiCompatible => "https://api.openai.com/v1",
        }
    }

    pub fn default_model_ids(&self) -> [&'static str; 4] {
        match self {
            // 顺序：fable → opus → sonnet → haiku（fable 复用 opus 档模型）
            Self::Anthropic => [
                "claude-opus-4-6",
                "claude-opus-4-6",
                "claude-sonnet-4-6",
                "claude-haiku-4-5-20251001",
            ],
            Self::OpenAiCompatible => ["gpt-5.5", "gpt-5.5", "gpt-4o", "gpt-4o-mini"],
        }
    }
}

// ── 单 Provider 配置 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigratedProvider {
    pub provider_type: ProviderType,
    pub provider_id: String,
    pub base_url: String,
    pub api_key: String,
    pub aliases: [String; 4],
    pub selected: bool,
}

impl MigratedProvider {
    pub fn new(pt: ProviderType) -> Self {
        Self {
            provider_type: pt,
            provider_id: pt.default_provider_id().to_string(),
            base_url: pt.default_base_url().to_string(),
            api_key: String::new(),
            aliases: pt.default_model_ids().map(|s| s.to_string()),
            selected: true,
        }
    }

    /// 字段是否完整
    pub fn is_complete(&self) -> bool {
        !self.provider_id.trim().is_empty()
            && !self.api_key.trim().is_empty()
            && self.aliases.iter().all(|a| !a.trim().is_empty())
    }

    /// 切换 Provider 类型后刷新默认值（保留 api_key）
    pub fn refresh_provider_defaults(&mut self) {
        self.provider_id = self.provider_type.default_provider_id().to_string();
        self.base_url = self.provider_type.default_base_url().to_string();
        self.aliases = self
            .provider_type
            .default_model_ids()
            .map(|s| s.to_string());
    }
}

// ── Form 模式 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormMode {
    Browse,
    Edit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormField {
    ProviderType,
    ProviderId,
    BaseUrl,
    TestConnectivity,
    ApiKey,
    FableModel,
    OpusModel,
    SonnetModel,
    HaikuModel,
    Confirm,
}

impl FormField {
    pub fn next(&self) -> Self {
        match self {
            Self::ProviderType => Self::ProviderId,
            Self::ProviderId => Self::BaseUrl,
            Self::BaseUrl => Self::TestConnectivity,
            Self::TestConnectivity => Self::ApiKey,
            Self::ApiKey => Self::FableModel,
            Self::FableModel => Self::OpusModel,
            Self::OpusModel => Self::SonnetModel,
            Self::SonnetModel => Self::HaikuModel,
            Self::HaikuModel => Self::Confirm,
            Self::Confirm => Self::ProviderType,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::ProviderType => Self::Confirm,
            Self::ProviderId => Self::ProviderType,
            Self::BaseUrl => Self::ProviderId,
            Self::TestConnectivity => Self::BaseUrl,
            Self::ApiKey => Self::TestConnectivity,
            Self::FableModel => Self::ApiKey,
            Self::OpusModel => Self::FableModel,
            Self::SonnetModel => Self::OpusModel,
            Self::HaikuModel => Self::SonnetModel,
            Self::Confirm => Self::HaikuModel,
        }
    }

    pub fn is_text_input(&self) -> bool {
        matches!(
            self,
            Self::ProviderId
                | Self::BaseUrl
                | Self::ApiKey
                | Self::FableModel
                | Self::OpusModel
                | Self::SonnetModel
                | Self::HaikuModel
        )
    }

    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::ProviderType => "setup-field-type",
            Self::ProviderId => "setup-field-id",
            Self::BaseUrl => "setup-field-base-url",
            Self::ApiKey => "setup-field-api-key",
            Self::TestConnectivity => "setup-field-test-connectivity",
            Self::FableModel => "setup-field-fable",
            Self::OpusModel => "setup-field-opus",
            Self::SonnetModel => "setup-field-sonnet",
            Self::HaikuModel => "setup-field-haiku",
            Self::Confirm => "setup-confirm",
        }
    }
}

// ── Wizard 完整状态 ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupWizardState {
    pub step: SetupStep,
    pub source: SetupSource,
    pub choose_cursor: usize,
    pub language: String,
    pub language_cursor: usize,
    pub providers: Vec<MigratedProvider>,
    pub active_provider: usize,
    pub form_mode: FormMode,
    pub browse_cursor: usize,
    pub form_focus: FormField,
    pub from_command: bool,
    pub submit_error: Option<String>,
    pub connectivity_result: Option<(bool, String)>,
    /// Edit 模式下当前文本框的光标位置（字符索引）
    pub edit_cursor_pos: usize,
}

impl Default for SetupWizardState {
    fn default() -> Self {
        Self {
            step: SetupStep::Language,
            source: SetupSource::CustomApi,
            choose_cursor: 0,
            language: "en".to_string(),
            language_cursor: 0,
            providers: vec![MigratedProvider::new(ProviderType::Anthropic)],
            active_provider: 0,
            form_mode: FormMode::Browse,
            browse_cursor: 0,
            form_focus: FormField::ProviderType,
            from_command: false,
            submit_error: None,
            connectivity_result: None,
            edit_cursor_pos: 0,
        }
    }
}

impl SetupWizardState {
    /// 获取当前活动 provider 的指定字段的可变引用
    pub fn active_provider_mut(&mut self) -> Option<&mut MigratedProvider> {
        self.providers.get_mut(self.active_provider)
    }

    /// 获取当前活动 provider 的不可变引用
    pub fn active_provider_ref(&self) -> Option<&MigratedProvider> {
        self.providers.get(self.active_provider)
    }

    /// 获取当前聚焦字段的文本值
    pub fn active_field_value(&self) -> Option<String> {
        let mp = self.active_provider_ref()?;
        Some(match self.form_focus {
            FormField::ProviderId => mp.provider_id.clone(),
            FormField::BaseUrl => mp.base_url.clone(),
            FormField::ApiKey => mp.api_key.clone(),
            FormField::FableModel => mp.aliases[0].clone(),
            FormField::OpusModel => mp.aliases[1].clone(),
            FormField::SonnetModel => mp.aliases[2].clone(),
            FormField::HaikuModel => mp.aliases[3].clone(),
            _ => return None,
        })
    }

    /// 设置当前聚焦字段的文本值
    pub fn set_active_field_value(&mut self, value: String) {
        let field = self.form_focus;
        if let Some(mp) = self.active_provider_mut() {
            match field {
                FormField::ProviderId => mp.provider_id = value,
                FormField::BaseUrl => mp.base_url = value,
                FormField::ApiKey => mp.api_key = value,
                FormField::FableModel => mp.aliases[0] = value,
                FormField::OpusModel => mp.aliases[1] = value,
                FormField::SonnetModel => mp.aliases[2] = value,
                FormField::HaikuModel => mp.aliases[3] = value,
                _ => {}
            }
        }
    }

    pub fn new_from_command() -> Self {
        Self {
            from_command: true,
            ..Self::default()
        }
    }
}

// ── 工具函数 ──────────────────────────────────────────────────────────────────

/// 检测是否需要 Setup 向导
pub fn needs_setup(config: &crate::config::AppConfig) -> bool {
    if config.providers.is_empty() {
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

/// API Key 脱敏显示
pub fn mask_api_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    let len = chars.len();
    if len <= 8 {
        "•".repeat(len)
    } else {
        let prefix: String = chars[..4].iter().collect();
        let suffix: String = chars[len - 4..].iter().collect();
        format!("{}••••{}", prefix, suffix)
    }
}

/// 从 wizard 数据构建 PeriConfig
pub fn build_wizard_config(state: &SetupWizardState) -> crate::config::PeriConfig {
    let mut cfg = crate::config::PeriConfig::default();
    let mut first_id = String::new();

    for mp in &state.providers {
        if !mp.selected {
            continue;
        }
        if mp.provider_id.trim().is_empty() || mp.api_key.trim().is_empty() {
            continue;
        }
        let provider = crate::config::ProviderConfig {
            id: mp.provider_id.clone(),
            provider_type: mp.provider_type.type_str().to_string(),
            api_key: mp.api_key.clone(),
            base_url: mp.base_url.clone(),
            models: crate::config::ProviderModels {
                fable: mp.aliases[0].clone(),
                opus: mp.aliases[1].clone(),
                sonnet: mp.aliases[2].clone(),
                haiku: mp.aliases[3].clone(),
            },
            ..Default::default()
        };
        if first_id.is_empty() {
            first_id = provider.id.clone();
        }
        cfg.config.providers.push(provider);
    }

    if !first_id.is_empty() {
        if state.source == SetupSource::PeriFreeService {
            apply_peri_free_profiles(&mut cfg.config, &first_id);
        } else {
            cfg.config.active_alias = "opus".to_string();
            if let Some(profile) = cfg.config.profiles.get_mut("opus") {
                profile.provider = first_id;
            }
        }
    }

    cfg.config.language = Some(state.language.clone());
    cfg
}

/// 应用 Peri Code 免费服务的档位配置（与 tavily-search/.peri/settings.json
/// 的 profiles 等同：active_alias=sonnet，各档 effort 固定，provider 绑定）。
fn apply_peri_free_profiles(cfg: &mut crate::config::AppConfig, provider_id: &str) {
    cfg.active_alias = "sonnet".to_string();
    for (alias, effort) in [
        ("fable", "max"),
        ("opus", "medium"),
        ("sonnet", "max"),
        ("haiku", "low"),
    ] {
        let profile = cfg.profiles.get_mut(alias).expect("固定四档 profile");
        profile.provider = provider_id.to_string();
        profile.effort = effort.to_string();
    }
}

/// 将 setup wizard 结果合并到已有配置并保存
pub fn save_setup(state: &SetupWizardState) -> anyhow::Result<crate::config::PeriConfig> {
    let mut merged = crate::config::load().unwrap_or_else(|_| crate::config::PeriConfig::default());

    let wizard_cfg = build_wizard_config(state);

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

    // 合并 wizard 中非默认档位的 profiles（Peri 免费服务会设置全部四档的 effort；
    // 其余来源仅设置 opus 的 provider，其余档位保持默认不覆盖）
    let wizard_active = wizard_cfg.config.active_alias.clone();
    let mut wizard_first_id = String::new();
    for alias in crate::config::Profiles::ALL {
        let Some(wp) = wizard_cfg.config.profiles.get(alias) else {
            continue;
        };
        if wp.is_default() {
            continue;
        }
        if wizard_first_id.is_empty() && !wp.provider.is_empty() {
            wizard_first_id = wp.provider.clone();
        }
        *merged
            .config
            .profiles
            .get_mut(alias)
            .expect("固定四档 profile") = wp.clone();
    }
    if !wizard_active.is_empty() {
        merged.config.active_alias = wizard_active;
    }
    if !wizard_first_id.is_empty()
        && let Some(profile) = merged.config.profiles.get_mut(&merged.config.active_alias)
    {
        profile.provider = wizard_first_id;
    }

    if let Some(lang) = wizard_cfg.config.language {
        merged.config.language = Some(lang);
    }

    crate::config::save_effective(&merged)?;
    Ok(merged)
}

/// 从 Claude Code 配置迁移
pub fn migrate_from_claude_code(
    state: &mut SetupWizardState,
    home_dir_override: Option<std::path::PathBuf>,
) -> bool {
    let home = home_dir_override
        .or_else(dirs_next::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let claude_dir = home.join(".claude");
    let settings_path = claude_dir.join("settings.json");
    if !settings_path.exists() {
        tracing::warn!("setup wizard: ~/.claude/settings.json 不存在，跳过 Claude Code 迁移");
        return false;
    }
    let content = match std::fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("setup wizard: 无法读取 ~/.claude/settings.json: {e}");
            return false;
        }
    };
    let val: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("setup wizard: ~/.claude/settings.json JSON 解析失败: {e}");
            return false;
        }
    };
    let env = match val.get("env").and_then(|e| e.as_object()) {
        Some(e) => e,
        None => {
            tracing::warn!("setup wizard: ~/.claude/settings.json 中缺少 env 字段");
            return false;
        }
    };

    let mut detected: Vec<MigratedProvider> = Vec::new();

    let prefixes: &[(&str, ProviderType, &str, &[&str])] = &[
        (
            "ANTHROPIC",
            ProviderType::Anthropic,
            "anthropic",
            &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
        ),
        (
            "OPENAI",
            ProviderType::OpenAiCompatible,
            "openai",
            &["OPENAI_API_KEY"],
        ),
        (
            "CODEX",
            ProviderType::OpenAiCompatible,
            "openai",
            &["CODEX_API_KEY"],
        ),
    ];

    for &(prefix, pt, default_id, key_names) in prefixes {
        let api_key = key_names
            .iter()
            .map(|k| get_env_string(env, k))
            .find(|v| !v.is_empty())
            .unwrap_or_default();
        let base_url = get_env_string(env, &format!("{}_BASE_URL", prefix));
        let fable = get_env_string(env, &format!("{}_DEFAULT_FABLE_MODEL", prefix));
        let opus = get_env_string(env, &format!("{}_DEFAULT_OPUS_MODEL", prefix));
        let sonnet = get_env_string(env, &format!("{}_DEFAULT_SONNET_MODEL", prefix));
        let haiku = get_env_string(env, &format!("{}_DEFAULT_HAIKU_MODEL", prefix));

        if api_key.is_empty() && base_url.is_empty() {
            continue;
        }

        let mut mp = MigratedProvider::new(pt);
        mp.provider_id = default_id.to_string();

        if !api_key.is_empty() {
            mp.api_key = api_key;
        } else {
            mp.selected = false;
        }

        if !base_url.is_empty() {
            mp.base_url = base_url;
        }

        if !fable.is_empty() {
            mp.aliases[0] = fable;
        }
        if !opus.is_empty() {
            mp.aliases[1] = opus;
        }
        if !sonnet.is_empty() {
            mp.aliases[2] = sonnet;
        }
        if !haiku.is_empty() {
            mp.aliases[3] = haiku;
        }

        detected.push(mp);
    }

    if detected.is_empty() {
        tracing::warn!(
            "setup wizard: ~/.claude/settings.json env 中未检测到任何已知 Provider 的 API Key"
        );
        return false;
    }

    tracing::info!(
        "setup wizard: 从 Claude Code 成功迁移 {} 个 Provider",
        detected.len()
    );
    state.providers = detected;
    state.active_provider = 0;
    state.browse_cursor = 0;
    true
}

fn get_env_string(env: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    match env.get(key) {
        Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
        Some(v) => {
            tracing::warn!(
                "setup wizard: env key '{}' has non-string value (type {:?}), skipping",
                key,
                v
            );
            String::new()
        }
        None => String::new(),
    }
}

/// 连通性测试
pub fn test_connectivity(base_url: &str) -> (bool, String) {
    use std::io::{Read, Write};

    if base_url.trim().is_empty() {
        return (false, "Base URL is empty".to_string());
    }

    let (host, port, path) = match parse_url_parts(base_url) {
        Some(p) => p,
        None => return (false, format!("Invalid URL: {}", base_url)),
    };

    let addr_str = format!("{}:{}", host, port);
    use std::net::ToSocketAddrs;
    let addr = match addr_str.to_socket_addrs().ok().and_then(|mut a| a.next()) {
        Some(a) => a,
        None => return (false, format!("DNS resolution failed for {}", host)),
    };

    let timeout = std::time::Duration::from_secs(5);
    let mut stream = match std::net::TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(e) => return (false, format!("{} unreachable: {}", host, e)),
    };
    let _ = stream.set_read_timeout(Some(timeout));

    let req = format!("GET {} HTTP/1.0\r\nHost: {}\r\n\r\n", path, host);
    if stream.write_all(req.as_bytes()).is_err() {
        return (false, format!("{} connected but send failed", host));
    }

    let mut buf = [0u8; 1];
    match stream.read_exact(&mut buf) {
        Ok(()) => (true, format!("{} reachable", base_url)),
        Err(e) => (false, format!("{} no response: {}", host, e)),
    }
}

fn parse_url_parts(url: &str) -> Option<(&str, u16, &str)> {
    let s = url.trim();
    let (scheme, rest) = if let Some(idx) = s.find("://") {
        (&s[..idx], &s[idx + 3..])
    } else {
        ("https", s)
    };
    let default_port: u16 = if scheme.eq_ignore_ascii_case("http") {
        80
    } else {
        443
    };
    let (host_port, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port_str) = match host_port.rfind(':') {
        Some(idx) if host_port[idx + 1..].chars().all(|c| c.is_ascii_digit()) => {
            (&host_port[..idx], &host_port[idx + 1..])
        }
        _ => (host_port, ""),
    };
    if host.is_empty() {
        return None;
    }
    let port: u16 = if port_str.is_empty() {
        default_port
    } else {
        port_str.parse().ok()?
    };
    Some((host, port, path))
}

#[cfg(test)]
#[path = "setup_wizard_test.rs"]
mod tests;
