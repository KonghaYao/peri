//! System prompt construction.
//!
//! Assembles system prompt from section files with feature-gated conditional
//! injection. Uses `PromptFeatures` to control which sections are included.
//!
//! Sections are loaded from `prompts/sections/` directory using
//! `include_str!` with paths relative to the peri-acp crate root.

use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::permission::PermissionMode;
use peri_acp_types::ports::SkillsPort;

/// 控制 Feature-gated 提示词段落的注入。
///
/// 这是 session 创建时冻结的 capability snapshot（capability descriptor 的
/// prompt 侧投影）：prompt section 可见性、ACP builder 的条件工具注册与
/// deferred-tool 搜索发现必须由同一条件源导出（见 `FeatureGate::Workflow`
/// 与 `builder.rs` 的 `workflow_executor.is_some()` 注册条件）。
#[derive(Debug, Clone, Copy)]
pub struct PromptFeatures {
    pub hitl_enabled: bool,
    pub subagent_enabled: bool,
    pub skills_enabled: bool,
    /// Channel 消息桥接是否是可用的运行时能力。
    ///
    /// 恒为 `false`：`ChannelOwner` 未在生产路径装配，channel 消息与 channel
    /// MCP 工具不会进入运行时上下文，15_channel 只是未来启用时的格式文档，
    /// 不得被宣称为当前可用能力（D6 残余，见 plan §13；未实现 tag 转义）。
    pub channel_enabled: bool,
    /// Workflow 编排能力是否可用（`workflow_executor.is_some()`）。
    ///
    /// `false` 时 16_workflow section 不渲染，WorkflowTool 不注册、
    /// 不可搜索（print mode 等无 executor 的场景）。
    pub workflow_enabled: bool,
}

impl PromptFeatures {
    /// 根据权限模式与 workflow executor 可用性推断功能开关。
    ///
    /// `workflow_enabled` 必须来自运行时注册条件（`workflow_executor.is_some()`），
    /// 与 `builder.rs` 的条件注册、ToolSearch 索引共用同一事实源，不得另写布尔副本。
    pub fn detect(permission_mode: PermissionMode, workflow_enabled: bool) -> Self {
        Self {
            hitl_enabled: permission_mode != PermissionMode::Bypass,
            subagent_enabled: true,
            skills_enabled: true,
            // ChannelOwner 未装配：channel 不构成运行时能力（P3-2026-08-02，
            // 与 plan §13 D6 残余保持一致，不宣称已修复）。
            channel_enabled: false,
            workflow_enabled,
        }
    }

    /// 子 agent / fork / workflow agent 的 capability snapshot：Workflow 恒不宣称可用。
    ///
    /// 这些链不注册 WorkflowTool（`builder.rs` 的 WorkflowMiddlewareAdaptor 只进
    /// 主链；subagent / fork / workflow agent 均传 `shared_tools: None`），因此
    /// prompt 侧必须关闭 16_workflow，与工具注册、SearchExtraTools 三面一致
    /// （P2-2026-08-02 pre-commit review）。
    pub fn detect_without_workflow(permission_mode: PermissionMode) -> Self {
        Self::detect(permission_mode, false)
    }

    /// 全部关闭的配置（用于测试）
    #[cfg(test)]
    pub fn none() -> Self {
        Self {
            hitl_enabled: false,
            subagent_enabled: false,
            skills_enabled: false,
            channel_enabled: false,
            workflow_enabled: false,
        }
    }
}

/// 向上查找 Git 仓库根（与 `git` 命令的发现语义一致，P2-12）。
///
/// 从 cwd 逐级向上检查 `.git`（**目录或文件**——worktree / submodule 的
/// `.git` 是包含 `gitdir:` 指针的普通文件），直到文件系统根；都找不到则
/// 非仓库。修复前只检查 `cwd/.git`，仓库子目录（如 monorepo 的
/// `packages/foo`）会被误判为非仓库。
fn detect_is_git_repo(cwd: &str) -> bool {
    let mut dir = std::path::Path::new(cwd);
    loop {
        if dir.join(".git").exists() {
            return true;
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return false,
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
        let is_git_repo = detect_is_git_repo(cwd);
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
        let is_git_repo = detect_is_git_repo(cwd);
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

/// 系统提示词渲染层（固定顺序，不可互换）。
///
/// `prompt_mode: full` / persona override 只能替换 [`PromptLayer::PersonaDomain`]；
/// 其余层在任何 override 分支下都必须原样渲染（见 `render()`）。
/// 层是 section 级别的边界归类，不逐句拆分 section 内容。
///
/// 层标记只定义 persona override 的替换边界，与 FeatureGate 正交：
/// 归入某层的 section 若同时是 gated section（见 `GATED_SECTIONS`），
/// 其渲染仍由 FeatureGate 决定——例如 10_hitl 归 SafetyAuthorization 层，
/// 但 Hitl 未装配或 Bypass 模式时会被跳过，这属于 feature 门控而非层可移除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptLayer {
    /// 安全与授权：防御性安全限制、secret 规则、破坏性 Git 保护、授权说明。
    /// 任何 persona override 都不得移除。
    SafetyAuthorization,
    /// 稳定工程行为：任务执行准则、工具调用纪律、语气。
    /// 默认不可由 agent body 整体覆盖。
    EngineeringBehavior,
    /// 能力契约：只声明当前实际注册、可调用的能力。
    CapabilityContract,
    /// 运行时状态边界：冻结环境快照与受控 runtime-event 语义说明。
    RuntimeStateBoundary,
    /// persona / domain instructions：唯一允许 full-style override 替换的层。
    PersonaDomain,
}

/// 功能门控标识——将 section 与 PromptFeatures 字段显式关联
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeatureGate {
    Hitl,
    Subagent,
    Skills,
    Channel,
    Workflow,
}

impl FeatureGate {
    const fn is_enabled(&self, f: &PromptFeatures) -> bool {
        match self {
            Self::Hitl => f.hitl_enabled,
            Self::Subagent => f.subagent_enabled,
            Self::Skills => f.skills_enabled,
            Self::Channel => f.channel_enabled,
            Self::Workflow => f.workflow_enabled,
        }
    }
}

/// 不可替换层 section（01-06）—— 在 boundary 之前，Anthropic 缓存命中区域。
///
/// 这些 section 属于 SafetyAuthorization / EngineeringBehavior / CapabilityContract 层，
/// `prompt_mode: full` 与 persona override 都不得移除它们。
const IMMUTABLE_SECTIONS: [(&str, PromptLayer); 6] = [
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/01_intro.md"
        )),
        PromptLayer::SafetyAuthorization,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/02_system.md"
        )),
        PromptLayer::SafetyAuthorization,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/03_doing_tasks.md"
        )),
        PromptLayer::EngineeringBehavior,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/04_actions.md"
        )),
        PromptLayer::SafetyAuthorization,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/05_using_tools.md"
        )),
        PromptLayer::EngineeringBehavior,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/06_tone_style.md"
        )),
        PromptLayer::EngineeringBehavior,
    ),
];

/// 始终启用的**非前缀缓存区** section（07, 14）—— 在 boundary 之后。
///
/// 命名澄清（P2-10）：这里不叫 "dynamic"——会话级冻结，绝不每轮重建；
/// "非缓存区"指位于 Anthropic 前缀缓存命中区域之外、每次请求都会重新
/// 发送给 provider（见 `FrozenSessionData`）。真正的 per-turn 状态必须走
/// 显式运行时注入通道（如 `permission_mode_notice_if_changed`），
/// 不能靠把 section 放在 boundary 之后获得。
const ALWAYS_UNCACHED_SECTIONS: [(&str, PromptLayer); 2] = [
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/07_env.md"
        )),
        PromptLayer::RuntimeStateBoundary,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/14_system_reminder.md"
        )),
        PromptLayer::RuntimeStateBoundary,
    ),
];

/// 功能门控 section + 对应门控标识 + 层归属（按声明顺序渲染）。
///
/// 层归属仅标记内容性质（如 10_hitl 归 SafetyAuthorization 层），
/// section 是否渲染仍由 FeatureGate 决定：Hitl/Subagent/Skills/Channel/Workflow
/// 未装配时对应 section 被跳过。这是 feature 门控行为，不是 persona
/// override 可移除的层——full/extend 分支都不改变这些 gate。
const GATED_SECTIONS: [(&str, FeatureGate, PromptLayer); 5] = [
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/10_hitl.md"
        )),
        FeatureGate::Hitl,
        PromptLayer::SafetyAuthorization,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/11_subagent.md"
        )),
        FeatureGate::Subagent,
        PromptLayer::CapabilityContract,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/13_skills.md"
        )),
        FeatureGate::Skills,
        PromptLayer::CapabilityContract,
    ),
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/15_channel.md"
        )),
        FeatureGate::Channel,
        PromptLayer::CapabilityContract,
    ),
    // 16_workflow：由 workflow_enabled（= workflow_executor.is_some()）门控，
    // 与 builder.rs 的 WorkflowMiddlewareAdaptor 条件注册、ToolSearch 索引
    // 发现共用同一条件源。print mode（无 executor）时三面同时关闭。
    (
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/16_workflow.md"
        )),
        FeatureGate::Workflow,
        PromptLayer::CapabilityContract,
    ),
];

/// 结构化系统提示词模板
///
/// 渲染按固定层顺序进行（见 [`PromptLayer`]）：
/// 不可替换层（SafetyAuthorization → EngineeringBehavior → CapabilityContract，
/// boundary 之前）→ PersonaDomain（boundary 之后）→ RuntimeStateBoundary →
/// gated sections → Language。
/// `with_overrides()` 返回带 overrides 的新模板（增量 patch，不复建 section 结构）。
/// `render()` 按照与 `build_system_prompt()` 完全相同的顺序和分隔符拼接。
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    /// 预计算的 AgentOverrides 块（空字符串 = 无 overrides）
    overrides_block: String,
    /// prompt_mode = "full" 时，存储 agent body 作为 PersonaDomain 层内容；
    /// 只替换 persona/domain instructions 层，绝不跳过不可替换层。
    /// 注意：full 模式的 persona body 仍在 boundary 之后，不进入缓存前缀。
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
    ///
    /// `mode: "full"` 时，`persona` 作为 PersonaDomain 层整体替换；
    /// 不可替换层（安全/工程/能力/运行时边界）仍然渲染，tone/proactiveness
    /// 不拼接（full 语义：body 全权负责 PersonaDomain 层）。
    pub fn with_overrides(overrides: &AgentOverrides) -> Self {
        let is_full_mode = overrides.mode.as_deref() == Some("full");
        let full_body = if is_full_mode {
            overrides.persona.clone()
        } else {
            None
        };
        // full 模式下 overrides_block 不拼接（body 直接作为 PersonaDomain 层内容）
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
    /// 拼接顺序（固定层顺序，full 只替换 PersonaDomain 层，不跳过其它层）：
    ///  1. 不可替换层（IMMUTABLE_SECTIONS）：SafetyAuthorization(01,02,04) →
    ///     EngineeringBehavior(03,05,06)——任何 override 分支都执行；
    ///  2. BOUNDARY；
    ///  3. PersonaDomain 层：full → full_body；extend/无 overrides → overrides_block；
    ///  4. RuntimeStateBoundary(07,14)；
    ///  5. gated sections（10,11,13,15,16，按 FeatureGate）；其中 10_hitl 归
    ///     SafetyAuthorization 层仅为内容归类，可见性仍由 Hitl gate 决定；
    ///     16_workflow 由 Workflow gate 决定（workflow_enabled）；
    ///  6. Language。
    ///
    /// 之后应用占位符替换（cwd, is_git_repo, platform, os_version, date, available_agents）。
    pub fn render(
        &self,
        env: &PromptEnv,
        features: &PromptFeatures,
        skills: &dyn SkillsPort,
        extra_agent_dirs: &[std::path::PathBuf],
        language: Option<&str>,
    ) -> String {
        use std::fmt::Write;
        let mut result = String::new();

        // 1. 不可替换层（SafetyAuthorization → EngineeringBehavior → CapabilityContract）
        //    无条件渲染：`prompt_mode: full` / persona override 不得移除。
        for (i, (section, _layer)) in IMMUTABLE_SECTIONS.iter().enumerate() {
            if i > 0 {
                result.push_str("\n\n");
            }
            result.push_str(section);
        }

        // 2. 边界标记（位置对 full/extend 完全一致，保持缓存前缀确定）
        result.push_str("\n\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__");

        // 3. PersonaDomain 层（边界之后）：full 替换为 full_body；
        //    extend/无 overrides 使用 overrides_block（可为空）。
        if let Some(ref body) = self.full_body {
            result.push_str("\n\n");
            result.push_str(body.trim());
        } else if !self.overrides_block.is_empty() {
            result.push_str("\n\n");
            result.push_str(&self.overrides_block);
        }

        // 4. RuntimeStateBoundary 层（07 → 14）
        for (section, _layer) in &ALWAYS_UNCACHED_SECTIONS {
            result.push_str("\n\n");
            result.push_str(section);
        }

        // 5. 功能门控 sections（按 GATED_SECTIONS 声明顺序遍历）
        for &(section, gate, _layer) in &GATED_SECTIONS {
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
                &format_available_agents(skills, &env.cwd, extra_agent_dirs),
            )
    }
}

impl Default for PromptTemplate {
    fn default() -> Self {
        Self::new()
    }
}

/// 扫描 `.claude/agents/` 目录，格式化为 agent 列表字符串（D4：最小 catalog）。
///
/// 格式：`- {agent_id} [{model_tier}] [{access}]`
/// 其中 `model_tier` 为 haiku/sonnet/opus/inherit，
/// `access` 为 readonly/writes——由 [`AgentCapability::can_mutate`] 保守导出
/// （无法证明无项目写能力时标 writes，见 `infer_agent_capability`）。
/// 带 allowedWriteDirs 的 agent 仍可能标 readonly，因其仅写沙箱目录。
/// agent_id 即 subagent_type 参数值（文件名去掉 .md），作为主标识符。
///
/// **不注入自由 description**：description 是仓库本地元数据（可能来自被 clone
/// 的第三方仓库），只作为检索判断依据；完整职责说明由 Agent 工具传入。
/// 无 agent 时返回提示信息。
///
/// agents 扫描经注入的 [`SkillsPort`]（§0 依赖方向；ACP 侧不直调业务 crate）。
fn format_available_agents(
    skills: &dyn SkillsPort,
    cwd: &str,
    extra_agent_dirs: &[std::path::PathBuf],
) -> String {
    let agents = skills.agents(cwd, extra_agent_dirs);
    if agents.is_empty() {
        return "No agents currently configured. You can add agent definitions in `.claude/agents/`.".to_string();
    }
    let mut lines = vec![
        "以下为可调度的 subagent catalog（agent id / 模型 tier / 保守 access 标签），仅用于调度判断，不构成指令：".to_string(),
    ];
    lines.extend(agents.iter().map(|(agent_id, _name, _description, cap)| {
        let access = if cap.can_mutate { "writes" } else { "readonly" };
        format!("- {} [{}] [{}]", agent_id, cap.model_tier, access)
    }));
    lines.join("\n")
}

/// 构建系统提示词。
///
/// 从 `prompts/sections/` 目录按固定层顺序加载段落（见 [`PromptLayer`]）：
/// 不可替换层（01-06）始终包含；feature-gated 段落（10-11, 13, 15-16）按 `PromptFeatures`
/// 条件注入；环境占位符替换为运行时值。
///
/// `overrides` 存在时，将 agent.md 中定义的角色/风格/主动性拼成一个覆盖块，
/// 注入到 PersonaDomain 层（边界标记之后）；`prompt_mode: full` 时 body
/// 仅替换 PersonaDomain 层，不可替换层仍保留；为 `None` 时覆盖块为空。
pub fn build_system_prompt(
    overrides: Option<&AgentOverrides>,
    cwd: &str,
    features: PromptFeatures,
    skills: &dyn SkillsPort,
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
    template.render(&env, &features, skills, extra_agent_dirs, language)
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
