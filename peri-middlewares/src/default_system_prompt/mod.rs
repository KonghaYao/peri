//! Frozen engineering defaults, persona, environment and language declarations.

use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::meta_harness::{sections, SectionSpec};
use peri_agent::middleware::prompt_sections::PromptSection;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::middleware::PromptSectionContent;
use std::fmt;
use std::fmt::{Debug, Formatter};

const BASE_SECTIONS: &[(SectionSpec, &str)] = &[
    (
        sections::INTRO,
        include_str!("../../prompts/sections/01_intro.md"),
    ),
    (
        sections::SYSTEM,
        include_str!("../../prompts/sections/02_system.md"),
    ),
    (
        sections::TASKS,
        include_str!("../../prompts/sections/03_doing_tasks.md"),
    ),
    (
        sections::ACTIONS,
        include_str!("../../prompts/sections/04_actions.md"),
    ),
    (
        sections::TOOLS,
        include_str!("../../prompts/sections/05_using_tools.md"),
    ),
    (
        sections::STYLE,
        include_str!("../../prompts/sections/06_tone_style.md"),
    ),
    (
        sections::RUNTIME,
        include_str!("../../prompts/sections/07_runtime.md"),
    ),
    (sections::ENV, include_str!("../../prompts/sections/env.md")),
];

/// 系统提示词基础段持有者（设计 §3.1.1 归属全景 / §3.5 演进 2）。
///
/// 持有 01-06 基础段、07_runtime 规则、env 环境正文与
/// persona 动态段（id `persona`：full → agent body；extend →
/// `build_agent_overrides_block`；无 overrides → 空内容不渲染——现状
/// render 三态语义保持）。关闭它 = 基础段 + Persona 全部消失。
pub struct DefaultSystemPromptMiddleware {
    overrides: Option<AgentOverrides>,
}

impl Debug for DefaultSystemPromptMiddleware {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DefaultSystemPromptMiddleware")
            .finish_non_exhaustive()
    }
}

impl DefaultSystemPromptMiddleware {
    /// 构造（`overrides` 为 agent.md / CLI --agent 覆盖项；None = 无 persona）。
    #[must_use]
    pub fn new(overrides: Option<AgentOverrides>) -> Self {
        Self { overrides }
    }

    /// 段落声明（渲染面收集与链收集的单一事实源）。
    ///
    /// persona 段**恒声明**（内容为空串时由渲染面空内容过滤跳过）——
    /// 保证 MetaHarness 覆盖 `.peri/meta/persona.md` 在无用户 overrides
    /// 时仍可注入（决策记录 D2）。
    #[must_use]
    pub fn sections(overrides: Option<&AgentOverrides>) -> Vec<PromptSection> {
        let mut sections = Vec::with_capacity(BASE_SECTIONS.len() + 1);
        sections.push(PromptSection::declared(
            sections::PERSONA,
            PromptSectionContent::Dynamic(build_persona_content(overrides)),
        ));
        for &(spec, content) in BASE_SECTIONS {
            sections.push(PromptSection::declared(
                spec,
                PromptSectionContent::Builtin(content),
            ));
        }
        sections
    }
}

impl Middleware for DefaultSystemPromptMiddleware {
    fn name(&self) -> &str {
        peri_acp_types::meta_harness::ChainSlot::DefaultSystemPrompt.name()
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
#[derive(Debug)]
pub struct LangMiddleware {
    language: Option<String>,
}

impl LangMiddleware {
    /// 构造（`language` 为冻结的 `settings.language`；None = 自动检测）。
    #[must_use]
    pub fn new(language: Option<String>) -> Self {
        Self { language }
    }

    /// 段落声明（渲染面收集与链收集的单一事实源）。
    ///
    /// language 段**恒声明**（无语言配置时内容为空串，由渲染面空内容
    /// 过滤跳过）——保证 `.peri/meta/language.md` 覆盖可注入。
    #[must_use]
    pub fn sections(language: Option<&str>) -> Vec<PromptSection> {
        vec![PromptSection::declared(
            sections::LANGUAGE,
            PromptSectionContent::Dynamic(build_language_content(language)),
        )]
    }
}

impl Middleware for LangMiddleware {
    fn name(&self) -> &str {
        peri_acp_types::meta_harness::ChainSlot::Lang.name()
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
