//! 配置类型定义 — 与 ~/.peri/settings.json 对应
//!
//! 从 peri-tui 迁移，移除 TUI 特有关联。

use std::collections::HashMap;

use peri_acp_types::meta_harness::{BUILT_IN_SUBAGENTS_KEY, MIDDLEWARE_NAMES, SECTION_IDS};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 顶层包装（与 ~/.peri/settings.json 的 { "config": {...} } 对应）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PeriConfig {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default)]
    pub config: AppConfig,
}

/// Provider 内的模型档位映射
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// 手动选择/输入的模型名；None 时回退到 provider.models 同档位映射
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// "low" | "medium" | "high" | "xhigh" | "max"
    #[serde(
        default = "default_profile_effort",
        skip_serializing_if = "is_default_effort"
    )]
    pub effort: String,
    /// 最大输出 token 数
    #[serde(
        default = "default_profile_max_tokens",
        skip_serializing_if = "is_default_max_tokens"
    )]
    pub max_tokens: u32,
    /// 是否启用 1M 上下文
    #[serde(default, skip_serializing_if = "is_false")]
    pub context_1m: bool,
}

/// 序列化辅助：effort 为默认值（"xhigh"）时不落盘——"默认值即未填写"
/// 与 merge 语义（非默认档位才覆盖）对称，保证工作区文件只含有效覆盖。
fn is_default_effort(v: &String) -> bool {
    *v == default_profile_effort()
}

fn is_default_max_tokens(v: &u32) -> bool {
    *v == default_profile_max_tokens()
}

fn is_false(v: &bool) -> bool {
    !*v
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
    /// 序列化辅助：与 `Profiles::default()` 对应档位相同即为"未填写"。
    /// merge 语义（非默认档位才覆盖全局）与序列化语义（默认档位不落盘）对称。
    pub fn is_default(&self) -> bool {
        self == &ProfileConfig::default()
    }
}

/// 固定四档 Profile（不可增删改名）
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
    /// 固定档位顺序（fable → opus → sonnet → haiku）。
    ///
    /// 档位集合须与契约层 `peri_acp_types::agents::MODEL_TIERS` 保持一致
    /// （顺序为该处弱 → 强展示序，此处强 → 弱为 UI/遍历语义）。
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BetasConfig {}

impl BetasConfig {
    /// 当前无任何开关（空结构）；恒为"未填写"，不落盘。
    pub fn is_default(&self) -> bool {
        true
    }
}

/// 应用配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    /// 当前激活的模型档位（"fable" | "opus" | "sonnet" | "haiku"）
    #[serde(default = "default_alias", skip_serializing_if = "String::is_empty")]
    pub active_alias: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    /// MetaHarness 控制字段：段落 ID → true（覆盖系统提示词段落）；
    /// middleware 名 → false（装配期关闭该 middleware）。
    ///
    /// 合并语义为 meta_harness **专属逐 key 合并**（`merge_overrides` 特例分支）：
    /// 项目级同 key 覆盖全局，全局其余 key 保留。无 null/删除语义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta_harness: Option<HashMap<String, bool>>,
    /// CLAUDE.md 排除 glob 模式列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_md_excludes: Option<Vec<String>>,
    /// 主动性级别
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactiveness: Option<String>,
    /// 是否在消息流中显示最终 prompt cache coverage 过低警告。
    /// Option<bool>：None=未设置（merge 时保留全局值），Some=显式开/关。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_cache_warning: Option<bool>,
    /// Beta 功能开关
    #[serde(default, skip_serializing_if = "BetasConfig::is_default")]
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
        // meta_harness 专属特例：逐 key 合并（项目级同 key 覆盖全局，全局其余
        // key 保留）。这是与 env 等整体覆盖字段刻意的行为差异（设计 §2.1）；
        // 不提供 null/删除语义。
        match (&mut self.meta_harness, workspace.meta_harness) {
            (Some(global), Some(workspace)) => global.extend(workspace),
            (None, Some(workspace)) => self.meta_harness = Some(workspace),
            (_, None) => {}
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

    /// 计算 `merged` 相对 `global` 的覆盖字段——[`merge_overrides`] 的严格逆操作。
    ///
    /// 分层写回契约：`ConfigSource::save` 写回工作区时只收录「与全局不同」的
    /// 字段，保证工作区文件保持"项目覆盖"性质（不拷贝全局字段/凭据，全局
    /// apiKey 不会进入项目文件），且与 `load` 对称：
    ///
    /// ```text
    /// merge_overrides(global, extract_overrides(merged, global)) == merged
    /// ```
    ///
    /// 规则与 [`merge_overrides`] 逐字段互逆：
    /// - 整体替换字段（providers / active_alias / profiles 档位 / Option 字段）：
    ///   `merged != global` 则收录 merged 值；
    /// - 逐 key 合并字段（meta_harness / extra）：收录 merged 中与 global 不同的
    ///   key 子集（含 global 未声明的 key）；
    /// - merge 未处理的字段（betas）：永不收录。
    pub fn extract_overrides(&self, global: &AppConfig) -> AppConfig {
        let mut ws = AppConfig::default();

        // providers — 整体替换字段
        if self.providers != global.providers {
            ws.providers = self.providers.clone();
        }
        // active_alias — 分层豁免：恒收录（与全局相同也写回）。
        //
        // 原因：解析期缺省为 "opus"（default_alias），无法区分「文件未声明」
        // 与「显式声明 opus」；若剔除该字段，工作区文件缺失时解析出 "opus"
        // 会经 merge 的"非空覆盖"错误覆盖全局的非 opus 值（roundtrip 破坏）。
        // 恒收录代价仅是工作区文件固定记录该字段，功能语义完全正确。
        ws.active_alias = self.active_alias.clone();
        // profiles — 逐档位整体替换（与 merge 的档位级语义对称）
        for alias in Profiles::ALL {
            let m = self.profiles.get(alias);
            let g = global.profiles.get(alias);
            if m != g {
                if let Some(m) = m {
                    *ws.profiles.get_mut(alias).unwrap() = m.clone();
                }
            }
        }
        // Option 字段 — 与全局不同则收录（含全局未声明）
        if self.skills_dir != global.skills_dir {
            ws.skills_dir = self.skills_dir.clone();
        }
        if self.env != global.env {
            ws.env = self.env.clone();
        }
        if self.compact != global.compact {
            ws.compact = self.compact.clone();
        }
        if self.language != global.language {
            ws.language = self.language.clone();
        }
        if self.persona != global.persona {
            ws.persona = self.persona.clone();
        }
        if self.tone != global.tone {
            ws.tone = self.tone.clone();
        }
        if self.claude_md_excludes != global.claude_md_excludes {
            ws.claude_md_excludes = self.claude_md_excludes.clone();
        }
        if self.proactiveness != global.proactiveness {
            ws.proactiveness = self.proactiveness.clone();
        }
        if self.show_cache_warning != global.show_cache_warning {
            ws.show_cache_warning = self.show_cache_warning;
        }
        // meta_harness — 专属逐 key 差异（与 merge 的逐 key 合并对称）
        match (&self.meta_harness, &global.meta_harness) {
            (Some(m), Some(g)) => {
                let diff: HashMap<String, bool> = m
                    .iter()
                    .filter(|(k, v)| g.get(*k) != Some(*v))
                    .map(|(k, v)| (k.clone(), *v))
                    .collect();
                if !diff.is_empty() {
                    ws.meta_harness = Some(diff);
                }
            }
            (Some(m), None) => ws.meta_harness = Some(m.clone()),
            (None, _) => {}
        }
        // extra — 逐 key 差异（与 merge 的 extend 对称）
        let extra_diff: Map<String, Value> = self
            .extra
            .iter()
            .filter(|(k, v)| global.extra.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !extra_diff.is_empty() {
            ws.extra = extra_diff;
        }
        ws
    }

    /// MetaHarness 解析期校验（warn 不 fail）：未知 key warn + 移除，已知 key
    /// 的四种 bool 组合全部保留。
    ///
    /// 只校验 key 集合与值语义，**不查文档**——文档存在性校验在冻结期
    /// （`build_frozen_data`），避免解析期二次读盘（设计 §2.1/2.3）。
    /// 由 `provider::store::load_from` 在每次 serde 解析成功后调用
    /// （生产路径唯一解析入口）。
    pub(crate) fn validate_meta_harness(&mut self) {
        let Some(map) = self.meta_harness.as_mut() else {
            return;
        };
        let known: std::collections::HashSet<&str> = SECTION_IDS
            .iter()
            .chain(MIDDLEWARE_NAMES.iter())
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
        // 高危组合检测（warn 不 fail，不改变值）：全部 middleware 均为 false
        // = 所有工具/钩子/段落持有者装配期被卸载。正常使用几乎不可能逐一手写
        // 全部 23 个 key，出现即高度疑似配置污染（曾发生过：项目级配置被写入
        // 全 false 后经 load() 合并透传写回全局配置，功能全关）。显著告警便于
        // 用户第一时间定位，而不是等会话静默降级。
        let all_middleware_disabled = MIDDLEWARE_NAMES
            .iter()
            .all(|name| map.get(*name).copied() == Some(false));
        if all_middleware_disabled {
            tracing::warn!(
                middleware_count = MIDDLEWARE_NAMES.len(),
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
            skills_dir: None,
            env: None,
            compact: None,
            language: None,
            persona: None,
            tone: None,
            meta_harness: None,
            proactiveness: None,
            claude_md_excludes: None,
            show_cache_warning: None,
            betas: BetasConfig::default(),
            extra: serde_json::Map::new(),
        }
    }
}

/// 单个 Provider 配置
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
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
