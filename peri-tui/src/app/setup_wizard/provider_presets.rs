//! 内置供应商预设表（base_url + 四档模型）。
//!
//! 供 Setup Wizard 的预设选择使用：每项描述一个开箱可选的供应商端点。
//! 顺序即展示顺序（国内官方 → 海外官方 → 聚合 → 免费网关），
//! 四档模型顺序固定 fable → opus → sonnet → haiku（与 `MigratedProvider::aliases` 一致）。
//! 摘要为固定中文内联文案，不随界面语言切换（TUI-I18N-001 记录的例外）。

use super::{MigratedProvider, ProviderType};

// ── 预设结构 ──────────────────────────────────────────────────────────────────

/// 单个内置供应商预设。
pub struct ProviderPreset {
    /// 展示名（品牌名）
    pub name: &'static str,
    /// 一行摘要（中文，内联文案，不进 i18n）
    pub summary: &'static str,
    /// Provider 协议类型
    pub provider_type: ProviderType,
    /// 写入配置的 provider id
    pub provider_id: &'static str,
    /// API 端点；末尾不带斜杠，Anthropic 兼容型的路径不含 `/v1/messages`
    pub base_url: &'static str,
    /// 预置 API Key（多数为空字符串，由用户自备）
    pub api_key: &'static str,
    /// 该预设要求固定档位配置：保存时由 `apply_fixed_tier_profiles` 写入
    /// active_alias 与各档 effort，不沿用默认的 opus 档位绑定
    pub fixed_tiers: bool,
    /// 四档模型，顺序固定 fable → opus → sonnet → haiku
    pub models: [&'static str; 4],
}

impl ProviderPreset {
    /// 转换为向导内部的 Provider 配置：以 `provider_type` 建立默认值后
    /// 逐字段覆盖，避免依赖 `MigratedProvider` 的私有字段。
    pub fn to_migrated(&self) -> MigratedProvider {
        let mut mp = MigratedProvider::new(self.provider_type);
        mp.provider_id = self.provider_id.to_string();
        mp.base_url = self.base_url.to_string();
        mp.api_key = self.api_key.to_string();
        mp.fixed_tiers = self.fixed_tiers;
        mp.aliases = self.models.map(str::to_string);
        mp
    }
}

// ── 预设表 ────────────────────────────────────────────────────────────────────

/// 内置供应商预设，顺序即展示顺序。
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "DeepSeek",
        summary: "DeepSeek 官方 Anthropic 兼容端点 · 需自备 API Key",
        provider_type: ProviderType::Anthropic,
        provider_id: "deepseek",
        base_url: "https://api.deepseek.com/anthropic",
        api_key: "",
        fixed_tiers: false,
        models: [
            "deepseek-v4-pro",
            "deepseek-v4-pro",
            "deepseek-flash",
            "deepseek-flash",
        ],
    },
    ProviderPreset {
        name: "Kimi",
        summary: "Kimi 官方 Anthropic 兼容端点 · 需自备 API Key",
        provider_type: ProviderType::Anthropic,
        provider_id: "kimi",
        base_url: "https://api.moonshot.cn/anthropic",
        api_key: "",
        fixed_tiers: false,
        models: ["kimi-k3", "kimi-k3", "kimi-k2.7-code", "kimi-k2.7-code"],
    },
    ProviderPreset {
        name: "智谱 GLM",
        summary: "智谱 GLM 官方 Anthropic 兼容端点 · 需自备 API Key",
        provider_type: ProviderType::Anthropic,
        provider_id: "zhipu",
        base_url: "https://open.bigmodel.cn/api/anthropic",
        api_key: "",
        fixed_tiers: false,
        models: ["glm-5.3", "glm-5.3", "glm-5.3-flash", "glm-5.3-flash"],
    },
    ProviderPreset {
        name: "MiniMax",
        summary: "MiniMax 官方 Anthropic 兼容端点 · 需自备 API Key",
        provider_type: ProviderType::Anthropic,
        provider_id: "minimax",
        base_url: "https://api.minimax.cn/anthropic",
        api_key: "",
        fixed_tiers: false,
        models: [
            "MiniMax-M3",
            "MiniMax-M3",
            "MiniMax-M2.7",
            "MiniMax-M2.7-highspeed",
        ],
    },
    ProviderPreset {
        name: "千问AI平台",
        summary: "千问AI平台 官方 Anthropic 兼容端点 · 需自备 API Key",
        provider_type: ProviderType::Anthropic,
        provider_id: "qwen",
        base_url: "https://dashscope.aliyuncs.com/apps/anthropic",
        api_key: "",
        fixed_tiers: false,
        models: [
            "qwen3.8-max",
            "qwen3.8-max",
            "qwen3.7-plus",
            "qwen3.8-flash",
        ],
    },
    ProviderPreset {
        name: "Anthropic 官方",
        summary: "Anthropic 官方端点 · 需自备 API Key",
        provider_type: ProviderType::Anthropic,
        provider_id: "anthropic",
        base_url: "https://api.anthropic.com",
        api_key: "",
        fixed_tiers: false,
        models: [
            "claude-fable-5-1",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
        ],
    },
    ProviderPreset {
        name: "OpenAI 官方",
        summary: "OpenAI 官方端点 · 需自备 API Key",
        provider_type: ProviderType::OpenAiCompatible,
        provider_id: "openai",
        base_url: "https://api.openai.com/v1",
        api_key: "",
        fixed_tiers: false,
        models: ["gpt-6-astra", "gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"],
    },
    ProviderPreset {
        name: "OpenRouter",
        summary: "聚合路由（Claude / DeepSeek）· 需自备 API Key",
        provider_type: ProviderType::Anthropic,
        provider_id: "openrouter",
        base_url: "https://openrouter.ai/api",
        api_key: "",
        fixed_tiers: false,
        models: [
            "anthropic/claude-fable-5.1",
            "anthropic/claude-opus-5.5",
            "deepseek/deepseek-v4.1-flash",
            "deepseek/deepseek-v4.1-flash",
        ],
    },
    ProviderPreset {
        name: "Peri Code 免费服务",
        summary: "Peri Code 公共网关 · 无需 API Key",
        provider_type: ProviderType::OpenAiCompatible,
        provider_id: "peri",
        base_url: "https://tavily.claude-code-best.win/peri-model/v1",
        api_key: "public",
        fixed_tiers: true,
        models: ["peri-fable", "peri-opus", "peri-sonnet", "peri-haiku"],
    },
];
