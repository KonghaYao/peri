//! System prompt construction.
//!
//! Assembles system prompt from section files with feature-gated conditional
//! injection. Uses `PromptFeatures` to control which sections are included.
//!
//! Sections are loaded from peri-tui's `prompts/sections/` directory using
//! `include_str!` with paths relative to the peri-acp crate root.

use peri_middlewares::AgentOverrides;

/// 控制 Feature-gated 提示词段落的注入
pub struct PromptFeatures {
    pub hitl_enabled: bool,
    pub subagent_enabled: bool,
    pub cron_enabled: bool,
    pub skills_enabled: bool,
    pub channel_enabled: bool,
}

impl PromptFeatures {
    /// 根据运行时环境推断功能开关
    pub fn detect() -> Self {
        Self {
            hitl_enabled: std::env::var("YOLO_MODE").as_deref() == Ok("false"),
            subagent_enabled: true,
            cron_enabled: true,
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
            cron_enabled: false,
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

/// 扫描 `.claude/agents/` 目录，格式化为 agent 列表字符串。
///
/// 格式：`- {agent_id} [{model_tier}] [{access}]: {description}`
/// 其中 `model_tier` 为 haiku/sonnet/opus/inherit，
/// `access` 为 readonly/writes（标识该 agent 是否会修改文件）。
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
/// 从 `prompts/sections/` 目录加载静态段落（01-07），根据 `PromptFeatures`
/// 条件注入 feature-gated 段落（10-13），将环境占位符替换为运行时值。
///
/// `overrides` 存在时，将 agent.md 中定义的角色/风格/主动性拼成一个覆盖块，
/// 注入到提示词最前面；为 `None` 时覆盖块为空（默认行为已由静态段落覆盖）。
pub fn build_system_prompt(
    overrides: Option<&AgentOverrides>,
    cwd: &str,
    features: PromptFeatures,
    extra_agent_dirs: &[std::path::PathBuf],
    frozen_date: Option<&str>,
    language: Option<&str>,
) -> String {
    let env = if let Some(date) = frozen_date {
        PromptEnv::with_frozen_date(cwd, date)
    } else {
        PromptEnv::detect(cwd)
    };

    // 静态段落（编译时嵌入，按编号顺序）—— 01-06 为缓存稳定内容
    // include_str! 路径相对于 source file，从 peri-acp/src/prompt/mod.rs 出发
    let static_sections: &[&str] = &[
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/01_intro.md"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/02_system.md"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/03_doing_tasks.md"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/04_actions.md"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/05_using_tools.md"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/06_tone_style.md"
        )),
    ];

    // 动态段落（含环境变量占位符、feature-gated 段落）—— 边界标记之后，不参与缓存
    let mut dynamic_sections: Vec<&str> = Vec::new();
    dynamic_sections.push(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../peri-tui/prompts/sections/07_env.md"
    )));
    dynamic_sections.push(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../peri-tui/prompts/sections/14_system_reminder.md"
    )));
    if features.hitl_enabled {
        dynamic_sections.push(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/10_hitl.md"
        )));
    }
    if features.subagent_enabled {
        dynamic_sections.push(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/11_subagent.md"
        )));
    }
    if features.cron_enabled {
        dynamic_sections.push(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/12_cron.md"
        )));
    }
    if features.skills_enabled {
        dynamic_sections.push(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/13_skills.md"
        )));
    }
    if features.channel_enabled {
        dynamic_sections.push(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../peri-tui/prompts/sections/15_channel.md"
        )));
    }

    let overrides_block = overrides
        .map(build_agent_overrides_block)
        .unwrap_or_default();

    // 合成：静态段落 + 边界标记 + 覆盖块 + 动态段落
    // 边界标记之前的全部内容可被 Anthropic prompt cache 命中；
    // 边界标记之后的内容（overrides、日期、cwd 等）变化不会破坏前缀缓存。
    // overrides_block 放在边界之后——不同 SubAgent 的 persona/tone 不同，
    // 若放在静态段之前会导致每个 agent 的缓存前缀完全失效。
    let mut result = String::new();
    for (i, section) in static_sections.iter().enumerate() {
        if i > 0 {
            result.push_str("\n\n");
        }
        result.push_str(section);
    }
    result.push_str("\n\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__");
    if !overrides_block.is_empty() {
        result.push_str("\n\n");
        result.push_str(&overrides_block);
    }
    for section in &dynamic_sections {
        result.push_str("\n\n");
        result.push_str(section);
    }
    // Language instruction (dynamic, after boundary to preserve cache prefix)
    if let Some(lang) = language {
        let lang_name = map_language_to_instruction(lang);
        result.push_str("\n\n# Language\n\n");
        result.push_str(&format!(
            "Always respond in {}. Use {} for all explanations, comments, and communications with the user. Technical terms and code identifiers should remain in their original form (e.g. API names, function/variable/type names, CLI tool names, library names, file paths, HTTP status codes, configuration keys, git commands).",
            lang_name, lang_name
        ));
    }

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
