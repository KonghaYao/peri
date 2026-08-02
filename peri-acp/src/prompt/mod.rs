//! System prompt construction.
//!
//! Assembles system prompt from section files with feature-gated conditional
//! injection. Uses `PromptFeatures` to control which sections are included.
//!
//! Sections are loaded from `prompts/sections/` directory using
//! `include_str!` with paths relative to the peri-acp crate root.

use peri_middlewares::{AgentOverrides, PermissionMode};

/// 控制 Feature-gated 提示词段落的注入
#[derive(Debug, Clone, Copy)]
pub struct PromptFeatures {
    pub hitl_enabled: bool,
    pub subagent_enabled: bool,
    pub skills_enabled: bool,
    pub channel_enabled: bool,
}

impl PromptFeatures {
    /// 根据权限模式推断功能开关
    pub fn detect(permission_mode: PermissionMode) -> Self {
        Self {
            hitl_enabled: permission_mode != PermissionMode::Bypass,
            subagent_enabled: true,
            skills_enabled: true,
            channel_enabled: true,
        }
    }

    /// 全部关闭的配置（用于测试）
    #[cfg(test)]
    pub fn none() -> Self {
        Self {
            hitl_enabled: false,
            subagent_enabled: false,
            skills_enabled: false,
            channel_enabled: false,
        }
    }
}

pub struct PromptEnv {
    pub cwd: String,
    pub is_git_repo: bool,
    pub platform: String,
    pub os_version: String,
    pub date: String,
}

impl PromptEnv {
    pub fn detect(cwd: &str) -> Self {
        let is_git_repo = std::path::Path::new(cwd).join(".git").exists();
        let platform = std::env::consts::OS.to_string();
        let os_version = os_version_string();
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        Self {
            cwd: cwd.to_string(),
            is_git_repo,
            platform,
            os_version,
            date,
        }
    }

    /// 使用冻结日期构造（跳过 `chrono::Local::now()` 调用）。
    /// `is_git_repo` 仍基于 cwd 实时检查；调用方若需冻结也应缓存。
    pub fn with_frozen_date(cwd: &str, frozen_date: &str) -> Self {
        let is_git_repo = std::path::Path::new(cwd).join(".git").exists();
        let platform = std::env::consts::OS.to_string();
        let os_version = os_version_string();
        Self {
            cwd: cwd.to_string(),
            is_git_repo,
            platform,
            os_version,
            date: frozen_date.to_string(),
        }
    }
}

/// 功能门控标识——将 section 与 PromptFeatures 字段显式关联
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeatureGate {
    Hitl,
    Subagent,
    Skills,
    Channel,
}

impl FeatureGate {
    const fn is_enabled(&self, f: &PromptFeatures) -> bool {
        match self {
            Self::Hitl => f.hitl_enabled,
            Self::Subagent => f.subagent_enabled,
            Self::Skills => f.skills_enabled,
            Self::Channel => f.channel_enabled,
        }
    }
}

/// 静态段（01-06, 16）—— 在 boundary 之前，Anthropic 缓存命中区域
const STATIC_SECTIONS: [&str; 7] = [
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/01_intro.md"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/02_system.md"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/03_doing_tasks.md"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/04_actions.md"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/05_using_tools.md"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/06_tone_style.md"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/16_workflow.md"
    )),
];

/// 始终启用的动态段（07, 14）—— 在 boundary 之后
const ALWAYS_DYNAMIC_SECTIONS: [&str; 2] = [
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/07_env.md"
    )),
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/14_system_reminder.md"
    )),
];

/// 功能门控动态段 + 对应门控标识（按 section 号顺序：10→11→13→15）
const GATED_SECTIONS: [(&str, FeatureGate); 4] = [
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/10_hitl.md"
        )),
        FeatureGate::Hitl,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/11_subagent.md"
        )),
        FeatureGate::Subagent,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/13_skills.md"
        )),
        FeatureGate::Skills,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/15_channel.md"
        )),
        FeatureGate::Channel,
    ),
];

/// 结构化系统提示词模板
///
/// 将 section 顺序、boundary 位置、feature gate 映射编码为数据。
/// `with_overrides()` 返回带 overrides 的新模板（增量 patch，不复建 section 结构）。
/// `render()` 按照与 `build_system_prompt()` 完全相同的顺序和分隔符拼接。
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    /// 预计算的 AgentOverrides 块（空字符串 = 无 overrides）
    overrides_block: String,
    /// prompt_mode = "full" 时，存储 agent body 作为 prompt 主体；
    /// 渲染时跳过 STATIC_SECTIONS，以 boundary + full_body + 动态段 拼接。
    /// 注意：full 模式下 Anthropic 前缀缓存必然 miss——这是用户显式选择的代价。
    full_body: Option<String>,
}

impl PromptTemplate {
    /// 创建基础模板（无 agent overrides）
    pub fn new() -> Self {
        Self {
            overrides_block: String::new(),
            full_body: None,
        }
    }

    /// 创建带 AgentOverrides 的模板。
    ///
    /// 用于 SubAgent define 路径：无需重建 section 结构，仅预计算 overrides 文本。
    /// 调用 `build_agent_overrides_block()`（与当前 build_system_prompt 使用同一函数）。
    pub fn with_overrides(overrides: &AgentOverrides) -> Self {
        let is_full_mode = overrides.mode.as_deref() == Some("full");
        let full_body = if is_full_mode {
            overrides.persona.clone()
        } else {
            None
        };
        // full 模式下 overrides_block 不拼接（body 直接作为 prompt 主体）
        let overrides_block = if is_full_mode {
            String::new()
        } else {
            build_agent_overrides_block(overrides)
        };
        Self {
            overrides_block,
            full_body,
        }
    }

    /// 渲染完整系统提示词
    ///
    /// 拼接顺序：
    ///   extend 模式：静态段(01-06,16) → BOUNDARY → [overrides] → 动态段(07,14) → 门控段(10-15) → [Language]
    ///   full 模式：BOUNDARY → full_body → 动态段(07,14) → 门控段(10-15) → [Language]
    ///
    /// 之后应用占位符替换（cwd, is_git_repo, platform, os_version, date, available_agents）。
    pub fn render(
        &self,
        env: &PromptEnv,
        features: &PromptFeatures,
        extra_agent_dirs: &[std::path::PathBuf],
        language: Option<&str>,
    ) -> String {
        use std::fmt::Write;
        let mut result = String::new();

        let is_full = self.full_body.is_some();

        // 静态段（01 → 02 → ... → 06 → 16）— full 模式跳过
        if !is_full {
            for (i, section) in STATIC_SECTIONS.iter().enumerate() {
                if i > 0 {
                    result.push_str("\n\n");
                }
                result.push_str(section);
            }
        }

        // 边界标记
        if !is_full {
            result.push_str("\n\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__");
        } else {
            result.push_str("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__");
        }

        // Agent overrides 块 / full_body（边界之后）
        if is_full {
            if let Some(ref body) = self.full_body {
                result.push_str("\n\n");
                result.push_str(body.trim());
            }
        } else if !self.overrides_block.is_empty() {
            result.push_str("\n\n");
            result.push_str(&self.overrides_block);
        }

        // 始终启用的动态段（07 → 14）
        for section in &ALWAYS_DYNAMIC_SECTIONS {
            result.push_str("\n\n");
            result.push_str(section);
        }

        // 功能门控动态段（按 GATED_SECTIONS 声明顺序遍历）
        for &(section, gate) in &GATED_SECTIONS {
            if gate.is_enabled(features) {
                result.push_str("\n\n");
                result.push_str(section);
            }
        }

        // Language 指令（动态，边界之后保留缓存前缀）
        if let Some(lang) = language {
            let lang_name = map_language_to_instruction(lang);
            result.push_str("\n\n# Language\n\n");
            let _ = write!(
                result,
                "Always respond in {}. Use {} for all explanations, comments, and communications with the user. Technical terms and code identifiers should remain in their original form (e.g. API names, function/variable/type names, CLI tool names, library names, file paths, HTTP status codes, configuration keys, git commands).",
                lang_name, lang_name
            );
        }

        // 占位符替换（顺序与 build_system_prompt 完全一致）
        result
            .replace("{{cwd}}", &env.cwd)
            .replace(
                "{{is_git_repo}}",
                if env.is_git_repo { "Yes" } else { "No" },
            )
            .replace("{{platform}}", &env.platform)
            .replace("{{os_version}}", &env.os_version)
            .replace("{{date}}", &env.date)
            .replace(
                "{{available_agents}}",
                &format_available_agents(&env.cwd, extra_agent_dirs),
            )
    }
}

impl Default for PromptTemplate {
    fn default() -> Self {
        Self::new()
    }
}

/// 扫描 `.claude/agents/` 目录，格式化为 agent 列表字符串。
///
/// 格式：`- {agent_id} [{model_tier}] [{access}]: {description}`
/// 其中 `model_tier` 为 haiku/sonnet/opus/inherit，
/// `access` 为 readonly/writes（标识该 agent 是否会修改项目代码。
/// 带 allowedWriteDirs 的 agent 仍标为 readonly，因其仅写沙箱目录）。
/// agent_id 即 subagent_type 参数值（文件名去掉 .md），作为主标识符。
/// 无 agent 时返回提示信息。
fn format_available_agents(cwd: &str, extra_agent_dirs: &[std::path::PathBuf]) -> String {
    let agents = peri_middlewares::scan_agents_detailed(cwd, extra_agent_dirs);
    if agents.is_empty() {
        return "No agents currently configured. You can add agent definitions in `.claude/agents/`.".to_string();
    }
    agents
        .iter()
        .map(|(agent_id, _name, description, cap)| {
            let access = if cap.can_mutate { "writes" } else { "readonly" };
            format!(
                "- {} [{}] [{}]: {}",
                agent_id, cap.model_tier, access, description
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 构建系统提示词。
///
/// 从 `prompts/sections/` 目录加载静态段落（01-06, 16），根据 `PromptFeatures`
/// 条件注入 feature-gated 段落（10-15），将环境占位符替换为运行时值。
///
/// `overrides` 存在时，将 agent.md 中定义的角色/风格/主动性拼成一个覆盖块，
/// 注入到边界标记之后；为 `None` 时覆盖块为空（默认行为已由静态段落覆盖）。
pub fn build_system_prompt(
    overrides: Option<&AgentOverrides>,
    cwd: &str,
    features: PromptFeatures,
    extra_agent_dirs: &[std::path::PathBuf],
    frozen_date: Option<&str>,
    language: Option<&str>,
) -> String {
    let template = overrides.map_or_else(PromptTemplate::new, PromptTemplate::with_overrides);
    let env = if let Some(date) = frozen_date {
        PromptEnv::with_frozen_date(cwd, date)
    } else {
        PromptEnv::detect(cwd)
    };
    template.render(&env, &features, extra_agent_dirs, language)
}

/// 将 `AgentOverrides` 拼成注入到提示词顶部的覆盖块。
///
/// 只包含非空字段，末尾加两个换行使其与后续默认内容自然分隔。
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

fn os_version_string() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
        {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !v.is_empty() {
                return format!("macOS {v}");
            }
        }
        "macOS".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/etc/os-release") {
            for line in s.lines() {
                if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
                    return v.trim_matches('"').to_string();
                }
            }
        }
        "Linux".to_string()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        std::env::consts::OS.to_string()
    }
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

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
