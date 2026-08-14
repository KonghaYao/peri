//! 系统提示词基础段持有者（波 4 演进 2，设计 §3.5 / §3.5.2）。
//!
//! 两个 middleware 持有系统提示词的基础段落（内容载体，设计 §3.5 语义边界
//! ①——段落渲染仍走 `PromptTemplate` 段落装配渲染，middleware 仅作内容
//! 载体，`prompt_contribution` 通道语义不变）：
//!
//! - [`DefaultSystemPromptMiddleware`]：01-06 基础段（Cached 1-6）+
//!   07_runtime（Uncached 1，07_env 与 14_system_reminder 合并）+ persona
//!   动态段（Uncached 0，full/extend/无 overrides 三态）。关闭它 =
//!   基础段 + Persona 全部消失（纯净模式清空 persona 覆盖）。
//! - [`LangMiddleware`]：Language 段（id `language`，Uncached 7，内容 =
//!   `settings.language` 映射的指令文本）。可关闭、可覆盖
//!   （`.peri/meta/language.md`）。
//!
//! 段落文件留在 `peri-acp/prompts/sections/`（设计 §3.5.1 步骤 3：文件可
//! 留在 sections/ 由 middleware `include_str!`，内容不复制）；本 crate 经
//! workspace 相对路径 `../peri-acp/prompts/sections/` 引用。
//!
//! 契约 3（gate 原子迁移）：本批迁移即判定——基础段 gate = 本 middleware
//! 是否在链上（收集即装配，`PromptTemplate` 侧收集段恒渲染）；渲染面构造
//! 点（冻结渲染 / 重渲染闭包）经 `DefaultSystemPromptMiddleware::sections`
//! 静态声明收集（冻结 disabled 集合驱动，链未装配也能得到一致段落——
//! C1 遗留问题 1/2 落定，见决策记录 D3）。本模块的段声明函数是渲染面与
//! 链收集的**单一事实源**，禁止双轨。
//!
//! 契约 4（运行时缺失防御）：middleware 在链上但内容为空（无 overrides /
//! 无语言配置）= 段仍声明、内容为空串，由渲染面空内容过滤跳过渲染不
//! fail——同时保证 `.peri/meta/persona.md` / `.peri/meta/language.md`
//! 覆盖在无用户配置时仍可注入（覆盖先于空过滤生效）。

use peri_acp_types::agents::AgentOverrides;
use peri_agent::middleware::{
    prompt_sections::{PromptSection, PromptSectionZone},
    r#trait::Middleware,
};

/// 基础段落声明（ID / 内容 / 位置属性）。
///
/// 01-06 在缓存区（Cached 1-6）；07_runtime 在非缓存区（Uncached 1）。
const BASE_SECTIONS: [(&str, &str, PromptSectionZone, u16); 7] = [
    (
        "01_intro",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-acp/prompts/sections/01_intro.md"
        )),
        PromptSectionZone::Cached,
        1,
    ),
    (
        "02_system",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-acp/prompts/sections/02_system.md"
        )),
        PromptSectionZone::Cached,
        2,
    ),
    (
        "03_doing_tasks",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-acp/prompts/sections/03_doing_tasks.md"
        )),
        PromptSectionZone::Cached,
        3,
    ),
    (
        "04_actions",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-acp/prompts/sections/04_actions.md"
        )),
        PromptSectionZone::Cached,
        4,
    ),
    (
        "05_using_tools",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-acp/prompts/sections/05_using_tools.md"
        )),
        PromptSectionZone::Cached,
        5,
    ),
    (
        "06_tone_style",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-acp/prompts/sections/06_tone_style.md"
        )),
        PromptSectionZone::Cached,
        6,
    ),
    (
        "07_runtime",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-acp/prompts/sections/07_runtime.md"
        )),
        PromptSectionZone::Uncached,
        1,
    ),
];

/// 系统提示词基础段持有者（设计 §3.1.1 归属全景 / §3.5 演进 2）。
///
/// 持有 01-06 基础段、07_runtime（07_env + 14_system_reminder 合并段）与
/// persona 动态段（id `persona`：full → agent body；extend →
/// `build_agent_overrides_block`；无 overrides → 空内容不渲染——现状
/// render 三态语义保持）。关闭它 = 基础段 + Persona 全部消失。
pub struct DefaultSystemPromptMiddleware {
    overrides: Option<AgentOverrides>,
}

impl DefaultSystemPromptMiddleware {
    /// 构造（`overrides` 为 agent.md / CLI --agent 覆盖项；None = 无 persona）。
    pub fn new(overrides: Option<AgentOverrides>) -> Self {
        Self { overrides }
    }

    /// 段落声明（渲染面收集与链收集的单一事实源）。
    ///
    /// persona 段**恒声明**（内容为空串时由渲染面空内容过滤跳过）——
    /// 保证 MetaHarness 覆盖 `.peri/meta/persona.md` 在无用户 overrides
    /// 时仍可注入（决策记录 D2）。
    pub fn sections(overrides: Option<&AgentOverrides>) -> Vec<PromptSection> {
        let mut sections = Vec::with_capacity(BASE_SECTIONS.len() + 1);
        sections.push(PromptSection::dynamic(
            "persona",
            PromptSectionZone::Uncached,
            0, // 现状顺序：boundary 后、07_runtime 之前
            build_persona_content(overrides),
        ));
        for (id, content, zone, order) in BASE_SECTIONS {
            sections.push(PromptSection::builtin(id, zone, order, content));
        }
        sections
    }
}

impl Middleware for DefaultSystemPromptMiddleware {
    fn name(&self) -> &str {
        "DefaultSystemPromptMiddleware"
    }

    /// 声明持有的系统提示词段落（内容载体；装配期收集，契约 2）。
    fn prompt_sections(&self) -> Vec<PromptSection> {
        Self::sections(self.overrides.as_ref())
    }
}

/// Language 段持有者（设计 §3.5.2 步骤 3）。
///
/// 持有 id `language` 的动态段（内容 = `settings.language` 映射的指令
/// 文本）；位置属性 = 非缓存区最后段（gated 之后，现状顺序）。可关闭
/// （`"LangMiddleware": false` → 语言段消失）、可覆盖
/// （`.peri/meta/language.md` 全文替换语言指令）。
pub struct LangMiddleware {
    language: Option<String>,
}

impl LangMiddleware {
    /// 构造（`language` 为冻结的 `settings.language`；None = 自动检测）。
    pub fn new(language: Option<String>) -> Self {
        Self { language }
    }

    /// 段落声明（渲染面收集与链收集的单一事实源）。
    ///
    /// language 段**恒声明**（无语言配置时内容为空串，由渲染面空内容
    /// 过滤跳过）——保证 `.peri/meta/language.md` 覆盖可注入。
    pub fn sections(language: Option<&str>) -> Vec<PromptSection> {
        vec![PromptSection::dynamic(
            "language",
            PromptSectionZone::Uncached,
            7, // gated 段（10/11/13/15 = 3-6）之后最后段
            build_language_content(language),
        )]
    }
}

impl Middleware for LangMiddleware {
    fn name(&self) -> &str {
        "LangMiddleware"
    }

    fn prompt_sections(&self) -> Vec<PromptSection> {
        Self::sections(self.language.as_deref())
    }
}

/// persona 段内容（full / extend / 无 overrides 三态，现状 render 语义
/// 保持，设计 §3.5.2 步骤 2）。
fn build_persona_content(overrides: Option<&AgentOverrides>) -> String {
    let Some(ov) = overrides else {
        return String::new();
    };
    if ov.mode.as_deref() == Some("full") {
        // full：agent body 作为 Persona 段整体（trim，现状 render 语义）
        ov.persona
            .as_deref()
            .map(|body| body.trim().to_string())
            .unwrap_or_default()
    } else {
        // extend / 默认：persona/tone/proactiveness 非空字段拼接
        build_agent_overrides_block(ov)
    }
}

/// 将 `AgentOverrides` 拼成 Persona 覆盖块（自 `peri-acp/src/prompt/mod.rs`
/// 随持有者迁入；只含非空字段，末尾加两个换行使其与后续默认内容自然分隔）。
fn build_agent_overrides_block(ov: &AgentOverrides) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(persona) = &ov.persona {
        parts.push(persona.trim().to_string());
    }
    if let Some(tone) = &ov.tone {
        parts.push(format!("# Tone and style\n{}", tone.trim()));
    }
    if let Some(proactiveness) = &ov.proactiveness {
        parts.push(format!("# Proactiveness\n{}", proactiveness.trim()));
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", parts.join("\n\n"))
    }
}

/// 语言指令全文（`map_language_to_instruction` 逻辑随持有者迁入；
/// 含 `# Language` 标题——段落即完整指令文本，覆盖 = 全文替换）。
fn build_language_content(language: Option<&str>) -> String {
    let Some(lang) = language else {
        return String::new();
    };
    let lang_name = map_language_to_instruction(lang);
    format!(
        "# Language\n\nAlways respond in {lang_name}. Use {lang_name} for all explanations, comments, and communications with the user. Technical terms and code identifiers should remain in their original form (e.g. API names, function/variable/type names, CLI tool names, library names, file paths, HTTP status codes, configuration keys, git commands)."
    )
}

/// Map language code to human-readable instruction string.
fn map_language_to_instruction(lang: &str) -> &str {
    match lang {
        "zh-CN" | "zh" => "Simplified Chinese",
        "zh-TW" => "Traditional Chinese",
        "ja" => "Japanese",
        "ko" => "Korean",
        _ => lang,
    }
}
