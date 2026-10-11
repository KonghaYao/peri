use std::collections::{BTreeMap, HashMap, HashSet};

use peri_acp_types::beta_flags;
use peri_acp_types::meta_harness::{
    BUILTIN_INSTANCE_POLICY_KEYS, BUILT_IN_SUBAGENTS_KEY, MIDDLEWARE_NAMES, SECTION_IDS,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PeriConfig {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default)]
    pub config: AppConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProviderModels {
    #[serde(default)]
    pub opus: String,
    #[serde(default)]
    pub sonnet: String,
    #[serde(default)]
    pub haiku: String,
    #[serde(default)]
    pub fable: String,
}

impl ProviderModels {
    pub fn get_model(&self, alias: &str) -> Option<&str> {
        match alias.to_lowercase().as_str() {
            "opus" => Some(&self.opus),
            "sonnet" => Some(&self.sonnet),
            "haiku" => Some(&self.haiku),
            "fable" => Some(if self.fable.is_empty() {
                &self.opus
            } else {
                &self.fable
            }),
            _ => None,
        }
    }
}

fn default_alias() -> String {
    "opus".to_owned()
}

fn default_profile_effort() -> String {
    "xhigh".to_owned()
}

fn default_profile_max_tokens() -> u32 {
    32000
}

fn is_default_effort(value: &String) -> bool {
    *value == default_profile_effort()
}

fn is_default_max_tokens(value: &u32) -> bool {
    *value == default_profile_max_tokens()
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(
        default = "default_profile_effort",
        skip_serializing_if = "is_default_effort"
    )]
    pub effort: String,
    #[serde(
        default = "default_profile_max_tokens",
        skip_serializing_if = "is_default_max_tokens"
    )]
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub context_1m: bool,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: None,
            effort: default_profile_effort(),
            max_tokens: default_profile_max_tokens(),
            context_1m: false,
        }
    }
}

impl ProfileConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Profiles {
    #[serde(default, skip_serializing_if = "ProfileConfig::is_default")]
    pub fable: ProfileConfig,
    #[serde(default, skip_serializing_if = "ProfileConfig::is_default")]
    pub opus: ProfileConfig,
    #[serde(default, skip_serializing_if = "ProfileConfig::is_default")]
    pub sonnet: ProfileConfig,
    #[serde(default, skip_serializing_if = "ProfileConfig::is_default")]
    pub haiku: ProfileConfig,
}

impl Profiles {
    pub const ALL: [&'static str; 4] = ["fable", "opus", "sonnet", "haiku"];

    pub fn get(&self, alias: &str) -> Option<&ProfileConfig> {
        match alias.to_lowercase().as_str() {
            "fable" => Some(&self.fable),
            "opus" => Some(&self.opus),
            "sonnet" => Some(&self.sonnet),
            "haiku" => Some(&self.haiku),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, alias: &str) -> Option<&mut ProfileConfig> {
        match alias.to_lowercase().as_str() {
            "fable" => Some(&mut self.fable),
            "opus" => Some(&mut self.opus),
            "sonnet" => Some(&mut self.sonnet),
            "haiku" => Some(&mut self.haiku),
            _ => None,
        }
    }
}

/// `config.betas`：flag id → bool 的稀疏覆盖表（序列化形状即 `{"<id>": true}`）。
///
/// 键必须在注册表（`peri_acp_types::beta_flags::BETA_FLAGS`）内；未知键在解析后由
/// [`Self::validate`] 从内存剔除并 warn（不进入快照、投影与面板），磁盘残留键在
/// 下一次经权威保存路径写回时清理。值必须是 bool：非 bool 使该来源解析失败，
/// 不静默降级。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct BetasConfig {
    pub overrides: BTreeMap<String, bool>,
}

impl BetasConfig {
    pub fn is_default(&self) -> bool {
        self.overrides.is_empty()
    }

    /// 已覆盖条目的值（未覆盖返回 `None`，与显式 `false` 区分）。
    pub fn get(&self, id: &str) -> Option<bool> {
        self.overrides.get(id).copied()
    }

    /// 写入覆盖（面板与装配层共用的唯一写入口）。
    pub fn set(&mut self, id: impl Into<String>, enabled: bool) {
        self.overrides.insert(id.into(), enabled);
    }

    /// 未知键剔除（`validate_meta_harness` 同款语义与接入点）。
    pub(crate) fn validate(&mut self) {
        self.overrides.retain(|id, _| {
            if beta_flags::find(id).is_some() {
                true
            } else {
                tracing::warn!(flag = %id, "config.betas: unknown flag id ignored");
                false
            }
        });
    }
}

/// 原始配置文档里仍存在的已删除 compact 键（判定与诊断分离，判定可测）。
///
/// 在**解析类型之前**调用：类型化模型已不认识这些键，只靠反序列化无法发现它们。
pub fn legacy_compact_keys_in(document: &serde_json::Value) -> Vec<&'static str> {
    let Some(compact) = document.pointer("/config/compact") else {
        return Vec::new();
    };
    peri_acp_types::compact::LEGACY_COMPACT_KEYS
        .into_iter()
        .filter(|key| compact.get(*key).is_some())
        .collect()
}

/// 对原始配置文档中已删除的 compact 键给出显式迁移诊断（当前无运行作用）。
pub fn warn_legacy_compact_keys(document: &serde_json::Value) {
    for key in legacy_compact_keys_in(document) {
        tracing::warn!(
            key,
            "compact 配置键已移除：{key} 从未参与运行，直接删除即可（本次按默认值继续，不静默换语义）"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_alias", skip_serializing_if = "String::is_empty")]
    pub active_alias: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub profiles: Profiles,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<peri_acp_types::compact::CompactConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta_harness: Option<HashMap<String, bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_md_excludes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactiveness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_cache_warning: Option<bool>,
    #[serde(default, skip_serializing_if = "BetasConfig::is_default")]
    pub betas: BetasConfig,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl AppConfig {
    pub fn merge_overrides(&mut self, workspace: AppConfig) {
        if !workspace.providers.is_empty() {
            self.providers = workspace.providers;
        }
        if !workspace.active_alias.is_empty() {
            self.active_alias = workspace.active_alias;
        }
        for alias in Profiles::ALL {
            if let Some(override_profile) = workspace.profiles.get(alias) {
                if override_profile != &ProfileConfig::default() {
                    if let Some(profile) = self.profiles.get_mut(alias) {
                        *profile = override_profile.clone();
                    }
                }
            }
        }
        match (&mut self.meta_harness, workspace.meta_harness) {
            (Some(global), Some(overrides)) => global.extend(overrides),
            (None, Some(overrides)) => self.meta_harness = Some(overrides),
            (_, None) => {}
        }
        // beta flag 覆盖逐 key 合并（与 MetaHarness 同款）：workspace 同名键胜出，
        // 显式 false 可关闭 global 的 true；不做整体替换。
        self.betas.overrides.extend(workspace.betas.overrides);
        if workspace.env.is_some() {
            self.env = workspace.env;
        }
        if workspace.compact.is_some() {
            self.compact = workspace.compact;
        }
        if workspace.language.is_some() {
            self.language = workspace.language;
        }
        if workspace.persona.is_some() {
            self.persona = workspace.persona;
        }
        if workspace.tone.is_some() {
            self.tone = workspace.tone;
        }
        if workspace.claude_md_excludes.is_some() {
            self.claude_md_excludes = workspace.claude_md_excludes;
        }
        if workspace.proactiveness.is_some() {
            self.proactiveness = workspace.proactiveness;
        }
        if workspace.show_cache_warning.is_some() {
            self.show_cache_warning = workspace.show_cache_warning;
        }
        self.extra.extend(workspace.extra);
    }

    pub fn extract_overrides(&self, global: &AppConfig) -> AppConfig {
        let mut overrides = AppConfig::default();
        if self.providers != global.providers {
            overrides.providers = self.providers.clone();
        }
        overrides.active_alias = self.active_alias.clone();
        for alias in Profiles::ALL {
            if self.profiles.get(alias) != global.profiles.get(alias) {
                if let Some(profile) = self.profiles.get(alias) {
                    *overrides.profiles.get_mut(alias).expect("known profile") = profile.clone();
                }
            }
        }
        if self.env != global.env {
            overrides.env = self.env.clone();
        }
        if self.compact != global.compact {
            overrides.compact = self.compact.clone();
        }
        if self.language != global.language {
            overrides.language = self.language.clone();
        }
        if self.persona != global.persona {
            overrides.persona = self.persona.clone();
        }
        if self.tone != global.tone {
            overrides.tone = self.tone.clone();
        }
        if self.claude_md_excludes != global.claude_md_excludes {
            overrides.claude_md_excludes = self.claude_md_excludes.clone();
        }
        if self.proactiveness != global.proactiveness {
            overrides.proactiveness = self.proactiveness.clone();
        }
        if self.show_cache_warning != global.show_cache_warning {
            overrides.show_cache_warning = self.show_cache_warning;
        }
        match (&self.meta_harness, &global.meta_harness) {
            (Some(current), Some(base)) => {
                let diff: HashMap<_, _> = current
                    .iter()
                    .filter(|(key, value)| base.get(*key) != Some(*value))
                    .map(|(key, value)| (key.clone(), *value))
                    .collect();
                if !diff.is_empty() {
                    overrides.meta_harness = Some(diff);
                }
            }
            (Some(current), None) => overrides.meta_harness = Some(current.clone()),
            (None, _) => {}
        }
        // beta flag 差异提取（与 MetaHarness 同款：只写与 global 不同的键）。
        overrides.betas.overrides = self
            .betas
            .overrides
            .iter()
            .filter(|(id, enabled)| global.betas.get(id) != Some(**enabled))
            .map(|(id, enabled)| (id.clone(), *enabled))
            .collect();
        overrides.extra = self
            .extra
            .iter()
            .filter(|(key, value)| global.extra.get(*key) != Some(*value))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        overrides
    }

    /// 解析后校验全部覆盖键（MetaHarness + beta flags）：未知键从内存剔除并 warn。
    pub(crate) fn validate_overrides(&mut self) {
        self.validate_meta_harness();
        self.betas.validate();
    }

    pub(crate) fn validate_meta_harness(&mut self) {
        let Some(map) = self.meta_harness.as_mut() else {
            return;
        };
        let known: HashSet<&str> = SECTION_IDS
            .iter()
            .chain(MIDDLEWARE_NAMES.iter())
            .chain(BUILTIN_INSTANCE_POLICY_KEYS.iter())
            .copied()
            .chain(std::iter::once(BUILT_IN_SUBAGENTS_KEY))
            .collect();
        map.retain(|key, _| {
            if known.contains(key.as_str()) {
                true
            } else {
                tracing::warn!(key = %key, "meta_harness: unknown key ignored");
                false
            }
        });
        let all_disabled = MIDDLEWARE_NAMES
            .iter()
            .chain(BUILTIN_INSTANCE_POLICY_KEYS.iter())
            .all(|name| map.get(*name).copied() == Some(false));
        if all_disabled {
            tracing::warn!(
                middleware_count = MIDDLEWARE_NAMES.len() + BUILTIN_INSTANCE_POLICY_KEYS.len(),
                "meta_harness: ALL middleware disabled — every tool/hook/section holder \
                 will be unavailable; if this was not intentional, remove the meta_harness \
                 keys from settings.json"
            );
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_alias: String::new(),
            providers: Vec::new(),
            profiles: Profiles::default(),
            env: None,
            compact: None,
            language: None,
            persona: None,
            tone: None,
            meta_harness: None,
            claude_md_excludes: None,
            proactiveness: None,
            show_cache_warning: None,
            betas: BetasConfig::default(),
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default)]
    pub provider_type: String,
    #[serde(rename = "apiKey", default)]
    pub api_key: String,
    #[serde(rename = "baseUrl", default)]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub models: ProviderModels,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ProviderConfig {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[cfg(test)]
#[path = "app_test.rs"]
mod tests;

#[cfg(test)]
#[path = "app_betas_test.rs"]
mod betas_tests;
