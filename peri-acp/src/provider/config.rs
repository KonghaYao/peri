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

/// Provider 内的三级别模型名映射
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderModels {
    #[serde(default)]
    pub opus: String,
    #[serde(default)]
    pub sonnet: String,
    #[serde(default)]
    pub haiku: String,
}

impl ProviderModels {
    /// 按 alias 名（大小写不敏感）获取对应模型名
    pub fn get_model(&self, alias: &str) -> Option<&str> {
        match alias.to_lowercase().as_str() {
            "opus" => Some(&self.opus),
            "sonnet" => Some(&self.sonnet),
            "haiku" => Some(&self.haiku),
            _ => None,
        }
    }
}

fn default_alias() -> String {
    "opus".to_string()
}

/// Thinking / 推理模式配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// 是否启用 thinking
    #[serde(default)]
    pub enabled: bool,
    /// 推理 token 预算
    #[serde(default = "default_budget_tokens")]
    pub budget_tokens: u32,
    /// 思考强度 "low" / "medium" / "high"
    #[serde(default = "default_effort")]
    pub effort: String,
    /// 最大输出 token 数
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_budget_tokens() -> u32 {
    8000
}

fn default_effort() -> String {
    "high".to_string()
}

fn default_max_tokens() -> u32 {
    32000
}

impl ThinkingConfig {
    /// 将 budget_tokens 映射到 OpenAI reasoning_effort 字符串
    pub fn openai_effort(&self) -> &str {
        &self.effort
    }

    // next_effort / prev_effort moved to peri-tui (TUI-only concern)
}

/// Beta 功能开关配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BetasConfig {}

/// 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 当前激活的模型别名（"opus" | "sonnet" | "haiku"）
    #[serde(default = "default_alias")]
    pub active_alias: String,
    /// 当前激活的 provider ID
    #[serde(default)]
    pub active_provider_id: String,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// 全局 skills 目录路径
    #[serde(default, alias = "skillsDir")]
    pub skills_dir: Option<String>,
    /// Thinking / 推理模式配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// 环境变量注入
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Compact 系统配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<peri_agent::agent::CompactConfig>,
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
    /// 是否启用 1M 上下文模式
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_1m: Option<bool>,
    /// 是否在消息流中显示缓存命中率过低警告
    #[serde(default = "default_show_cache_warning")]
    pub show_cache_warning: bool,
    /// Beta 功能开关
    #[serde(default)]
    pub betas: BetasConfig,
    /// 保留未知字段
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_show_cache_warning() -> bool {
    true
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
        if !workspace.active_provider_id.is_empty() {
            self.active_provider_id = workspace.active_provider_id;
        }
        // Option<T> 字段 — is_some() 则覆盖
        if workspace.skills_dir.is_some() {
            self.skills_dir = workspace.skills_dir;
        }
        if workspace.thinking.is_some() {
            self.thinking = workspace.thinking;
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
        if workspace.context_1m.is_some() {
            self.context_1m = workspace.context_1m;
        }
        // show_cache_warning: bool 直接覆盖
        self.show_cache_warning = workspace.show_cache_warning;
        // 保留未知字段
        self.extra.extend(workspace.extra);
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_alias: String::new(),
            active_provider_id: String::new(),
            providers: Vec::new(),
            skills_dir: None,
            thinking: None,
            env: None,
            compact: None,
            language: None,
            persona: None,
            tone: None,
            proactiveness: None,
            context_1m: None,
            claude_md_excludes: None,
            show_cache_warning: true,
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
    /// Provider 级别的 ThinkingConfig
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
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
