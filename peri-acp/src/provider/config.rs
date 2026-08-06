//! 配置类型定义 — 与 ~/.peri/settings.json 对应
//!
//! 从 peri-tui 迁移，移除 TUI 特有关联。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 顶层包装（与 ~/.peri/settings.json 的 { "config": {...} } 对应）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeriConfig {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default)]
    pub config: AppConfig,
}

/// Provider 内的模型档位映射
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderModels {
    #[serde(default)]
    pub opus: String,
    #[serde(default)]
    pub sonnet: String,
    #[serde(default)]
    pub haiku: String,
    /// fable 档位模型名；为空时回退到 opus 档位
    #[serde(default)]
    pub fable: String,
}

impl ProviderModels {
    /// 按 alias 名（大小写不敏感）获取对应模型名；fable 档位为空时回退 opus
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
    "opus".to_string()
}

fn default_profile_effort() -> String {
    "xhigh".to_string()
}

fn default_profile_max_tokens() -> u32 {
    32000
}

/// 单个 Profile 的独立配置（请求参数唯一事实源）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileConfig {
    /// 引用 providers[].id；空字符串表示未绑定 provider（请求时回退第一个可用 provider）
    #[serde(default)]
    pub provider: String,
    /// 手动选择/输入的模型名；None 时回退到 provider.models 同档位映射
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// "low" | "medium" | "high" | "xhigh" | "max"
    #[serde(default = "default_profile_effort")]
    pub effort: String,
    /// 最大输出 token 数
    #[serde(default = "default_profile_max_tokens")]
    pub max_tokens: u32,
    /// 是否启用 1M 上下文
    #[serde(default)]
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

/// 固定四档 Profile（不可增删改名）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profiles {
    #[serde(default)]
    pub fable: ProfileConfig,
    #[serde(default)]
    pub opus: ProfileConfig,
    #[serde(default)]
    pub sonnet: ProfileConfig,
    #[serde(default)]
    pub haiku: ProfileConfig,
}

impl Profiles {
    /// 固定档位顺序（fable → opus → sonnet → haiku）
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

/// Beta 功能开关配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BetasConfig {}

/// 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 当前激活的模型档位（"fable" | "opus" | "sonnet" | "haiku"）
    #[serde(default = "default_alias")]
    pub active_alias: String,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// 四档 Profile（请求参数唯一事实源）
    #[serde(default)]
    pub profiles: Profiles,
    /// 全局 skills 目录路径
    #[serde(default, alias = "skillsDir")]
    pub skills_dir: Option<String>,
    /// 环境变量注入
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Compact 系统配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<peri_acp_types::compact::CompactConfig>,
    /// UI 语言
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// 系统提示词 persona 覆盖
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// 系统提示词 tone 覆盖
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    /// CLAUDE.md 排除 glob 模式列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_md_excludes: Option<Vec<String>>,
    /// 主动性级别
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactiveness: Option<String>,
    /// 是否在消息流中显示缓存命中率过低警告。
    /// Option<bool>：None=未设置（merge 时保留全局值），Some=显式开/关。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_cache_warning: Option<bool>,
    /// Beta 功能开关
    #[serde(default)]
    pub betas: BetasConfig,
    /// 保留未知字段（旧 thinking/active_provider_id/context_1m 会被吸收到此，不回写）
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl AppConfig {
    /// 用 workspace 配置覆盖全局配置。
    /// workspace 中出现的字段替换全局对应字段，未出现的保留全局值。
    pub fn merge_overrides(&mut self, workspace: AppConfig) {
        // providers — 空列表视为"未填写"，不覆盖
        if !workspace.providers.is_empty() {
            self.providers = workspace.providers;
        }
        // 字符串字段 — 非空则覆盖
        if !workspace.active_alias.is_empty() {
            self.active_alias = workspace.active_alias;
        }
        // Profile — 项目级存在某档位且非默认 → 整体替换（不做字段级合并）；
        // 项目级不存在（或等于默认值）→ 该档位保留全局完整配置。
        for alias in Profiles::ALL {
            if let Some(ws) = workspace.profiles.get(alias) {
                if ws != &ProfileConfig::default() {
                    if let Some(global) = self.profiles.get_mut(alias) {
                        *global = ws.clone();
                    }
                }
            }
        }
        // Option<T> 字段 — is_some() 则覆盖
        if workspace.skills_dir.is_some() {
            self.skills_dir = workspace.skills_dir;
        }
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
        // show_cache_warning: 仅当 workspace 显式设置时才覆盖（避免默认 false 冲掉全局开启）
        if workspace.show_cache_warning.is_some() {
            self.show_cache_warning = workspace.show_cache_warning;
        }
        // 保留未知字段
        self.extra.extend(workspace.extra);
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_alias: String::new(),
            providers: Vec::new(),
            profiles: Profiles::default(),
            skills_dir: None,
            env: None,
            compact: None,
            language: None,
            persona: None,
            tone: None,
            proactiveness: None,
            claude_md_excludes: None,
            show_cache_warning: None,
            betas: BetasConfig::default(),
            extra: serde_json::Map::new(),
        }
    }
}

/// 单个 Provider 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    #[serde(default)]
    pub id: String,
    /// "openai" | "anthropic" 等
    #[serde(rename = "type", default)]
    pub provider_type: String,
    #[serde(rename = "apiKey", default)]
    pub api_key: String,
    /// OpenAI Base URL
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
#[path = "config_test.rs"]
mod tests;
