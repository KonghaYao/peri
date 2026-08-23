//! System prompt construction.
//!
//! Assembles system prompt from section files with feature-gated conditional
//! injection. Uses `PromptFeatures` to control which sections are included.
//!
//! Sections are loaded from `prompts/sections/` directory using
//! `include_str!` with paths relative to the peri-acp crate root.

use std::sync::Arc;

use peri_acp_types::ports::SkillsPort;
use peri_agent::middleware::{PromptSection, PromptSectionContent, PromptSectionZone};

/// 控制 Feature-gated 提示词段落的注入。
///
/// 这是 session 创建时冻结的 capability snapshot（capability descriptor 的
/// prompt 侧投影）：prompt section 可见性、ACP builder 的条件工具注册与
/// deferred-tool 搜索发现必须由同一条件源导出。
///
/// 波 4 演进（C2/C3）：基础段（01-06 / 07_runtime / persona / language）与
/// gated 段（10_hitl / 11_subagent / 13_skills）已全部迁移至功能 middleware
/// 持有——gate = 持有者是否在链上（收集即装配，契约 3），不再经本结构
/// 判定；`permission_mode` 不再参与任何 gate 判定。本结构仅保留
/// 15_channel 的硬编码判定（无持有 middleware，gate 恒 false 直至未来
/// channel middleware 装配，设计 §3.1.1）。
#[derive(Debug, Clone, Copy)]
pub struct PromptFeatures {
    /// Channel 消息桥接是否是可用的运行时能力。
    ///
    /// 恒为 `false`：`ChannelOwner` 未在生产路径装配，channel 消息与 channel
    /// MCP 工具不会进入运行时上下文，15_channel 只是未来启用时的格式文档，
    /// 不得被宣称为当前可用能力（D6 残余，见 plan §13；未实现 tag 转义）。
    pub channel_enabled: bool,
}

impl PromptFeatures {
    /// 生产默认配置（仅 channel gate：15_channel 无持有者，恒关闭）。
    ///
    /// 波 4 演进（C3，决策记录 C3 D4）：hitl/subagent/skills gate 已随段落
    /// 实体迁移至「持有 middleware 是否在链上」（收集即装配，契约 3），
    /// `permission_mode` 不再是 gate 判定输入——签名无参化。
    pub fn detect() -> Self {
        Self {
            // ChannelOwner 未装配：channel 不构成运行时能力（P3-2026-08-02，
            // 与 plan §13 D6 残余保持一致，不宣称已修复）。
            channel_enabled: false,
        }
    }

    /// 全部关闭的配置（用于测试；与 `detect` 语义等价——仅 channel gate，
    /// 恒 false）
    #[cfg(test)]
    pub fn none() -> Self {
        Self {
            channel_enabled: false,
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

/// 功能门控标识——将 section 与 PromptFeatures 字段显式关联。
///
/// 波 4 演进（C3）：Hitl/Subagent/Skills 变体已随段落实体迁移删除
/// （gate = 持有 middleware 是否在链上，收集即装配，契约 3）；仅剩
/// Channel（15_channel 无持有者，gate 恒 false）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeatureGate {
    Channel,
}

impl FeatureGate {
    const fn is_enabled(&self, f: &PromptFeatures) -> bool {
        match self {
            Self::Channel => f.channel_enabled,
        }
    }
}

/// 功能门控 section + 对应门控标识（按声明顺序渲染）。
///
/// section 是否渲染由 FeatureGate 决定：Channel 未装配时对应 section 被
/// 跳过。这是 feature 门控行为——full/extend 分支都不改变这些 gate。
///
/// 元素形态：(ID, 内容, Gate, 段内序号)。ID 供 MetaHarness 段落覆盖定位
/// （设计 §2.4）。
///
/// 波 4 演进（C2/C3）：基础段（01-06 / 07_runtime / persona / language）
/// 与 gated 段（10_hitl / 11_subagent / 13_skills）已迁移至 middleware
/// 持有（`DefaultSystemPromptMiddleware` / `LangMiddleware` /
/// `PermissionMiddleware` / `SubAgentMiddleware` / `SkillsMiddleware`），
/// 本数组仅剩无持有者的 15_channel（非缓存区段内序号 7，07_runtime=1 与
/// 已迁移 gated 10=3/11=4/12=5/13=6 之后、language=8 之前——编号不重排，
/// C1 D2 编号事实；2026-08-15 职责拆分新增 12_ask_user=5 后 13/15/language
/// 序号顺延，见 `spec/issues/2026-08-15-permission-hitl-split.md`）；
/// 16_workflow 已整段删除（ultracode skill 完整覆盖，设计 §3.1.2）。
type GatedSection = (&'static str, &'static str, FeatureGate, u16);

const GATED_SECTIONS: [GatedSection; 1] = [(
    "15_channel",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/prompts/sections/15_channel.md"
    )),
    FeatureGate::Channel,
    7,
)];

/// 结构化系统提示词模板
///
/// 渲染按固定段落顺序进行：缓存区段落（zone=Cached，01-06）→ 非缓存区
/// 段落（zone=Uncached：persona → 07_runtime → gated 段落 → language，
/// 按段内序号 + gate 判定）。`render()` 按"位置 + 段内序号"顺序拼接
/// （构造期物化，与全部构造点一致的顺序和分隔符）。
///
/// MetaHarness（设计 §2.4）：`new(state, collected)` 构造期按段落 ID 将覆盖
/// 内容合入 resolved sections——render 阶段只迭代已解析内容，无查表开销、
/// 不读盘。
///
/// 波 4 演进（设计 §3.1.1 拆分持有契约 2/4）：`collected` 为收集的
/// middleware 持有段落（链侧 `MiddlewareChain::collect_prompt_sections` /
/// 渲染面 `crate::session::build_collected_sections` 静态声明——渲染面
/// 收集位于 ACP 宿主装配面 `session/mod.rs`，§0 边 2 豁免）——按 ID 覆盖
/// 编译期内置段落（位置属性以持有者声明为准，gate 随收集即装配）；C2/C3
/// 起基础段（01-06 / 07_runtime / persona / language）与 gated 段
/// （10_hitl / 11_subagent / 13_skills）唯一来源为收集结果；内置数组仅剩
/// 无持有者的
/// 15_channel（gate 恒 false，C3 后状态）。
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    /// 已解析的缓存区段落（zone=Cached，01-06，按段内序号升序）。
    cached_sections: Vec<ResolvedSection>,
    /// 已解析的非缓存区段落（zone=Uncached：persona + 07_runtime + gated +
    /// 收集段 + language，按段内序号升序）。
    uncached_sections: Vec<ResolvedSection>,
    /// available agents catalog 是否包含 compile-time built-ins（会话冻结）。
    built_in_subagents_enabled: bool,
}

/// 段落内容来源（实现裁定 Q2：内置静态文本零拷贝借用，覆盖全文持 Arc）。
#[derive(Debug, Clone)]
enum SectionContent {
    /// 内置段落（`include_str!` 静态文本，零拷贝）
    Builtin(&'static str),
    /// MetaHarness 覆盖全文（冻结期扫描 `.peri/meta/<id>.md`）
    Override(Arc<str>),
    /// middleware 动态生成的段落全文（装配期收集，`PromptSectionContent::Dynamic`）
    Dynamic(String),
}

impl SectionContent {
    /// 渲染用文本视图
    fn as_str(&self) -> &str {
        match self {
            Self::Builtin(s) => s,
            Self::Override(s) => s,
            Self::Dynamic(s) => s,
        }
    }
}

/// 段落渲染条件（gate 判定来源）。
///
/// C3 后语义（设计 §3.1.1 拆分持有契约 3）：middleware 收集段落（持有者
/// 已在链上）恒渲染（收集即装配）；内置数组仅剩 15_channel（无持有者），
/// gate 由 [`PromptFeatures`] 硬编码判定（恒 false）。
#[derive(Debug, Clone, Copy)]
enum SectionGate {
    /// 恒渲染（middleware 收集段）
    Always,
    /// 由 [`PromptFeatures`] 字段硬编码判定（15_channel，无持有者）
    Feature(FeatureGate),
}

impl SectionGate {
    const fn is_enabled(&self, f: &PromptFeatures) -> bool {
        match self {
            Self::Always => true,
            Self::Feature(gate) => gate.is_enabled(f),
        }
    }
}

/// 构造期解析后的段落（`id` 为段落覆盖与持有权迁移的定位键，渲染只消费
/// zone/order/content/gate）。
#[derive(Debug, Clone)]
struct ResolvedSection {
    id: &'static str,
    zone: PromptSectionZone,
    order: u16,
    content: SectionContent,
    gate: SectionGate,
}

impl PromptTemplate {
    /// 创建基础模板（无 agent overrides）。
    ///
    /// 构造期物化：
    /// 1. 编译期内置数组（`GATED_SECTIONS`，仅无持有者的 15_channel）→
    ///    带位置属性（zone + 段内序号）与 gate 的段落声明；
    /// 2. `collected`（middleware 持有段落：链收集 `collect_prompt_sections`
    ///    / 渲染面静态声明 `crate::session::build_collected_sections`）按 ID
    ///    覆盖内置段落——
    ///    位置属性以持有者声明为准，gate = Always（收集即持有者已装配，
    ///    契约 3）；C2/C3 起基础段（01-06 / 07_runtime / persona / language）
    ///    与 gated 段（10_hitl / 11_subagent / 13_skills）唯一来源为收集
    ///    结果（数组已删除，禁止双轨）；
    /// 3. `state.section_overrides` 按 ID 替换内容（覆盖 = 替换持有者对应段落
    ///    贡献，机制与持有者无关，设计 §2.4）；
    /// 4. 空内容段落过滤（契约 4：未提供内容 = 跳过渲染不 fail），按
    ///    "位置 + 段内序号"排序（契约 2：不依赖 middleware 链序）。
    ///
    /// 改动收敛到 `new` 一处：render 签名与全部调用点分隔符语义不变。
    pub fn new(
        state: &peri_acp_types::meta_harness::MetaHarnessState,
        collected: &[PromptSection],
    ) -> Self {
        // 1. 内置数组 → 段落声明（ID 即文件名去 .md，位置属性 + gate 显式化）
        let mut sections: Vec<ResolvedSection> = Vec::with_capacity(1 + collected.len());
        for (id, content, gate, order) in GATED_SECTIONS.iter() {
            sections.push(ResolvedSection {
                id,
                zone: PromptSectionZone::Uncached,
                // 非缓存区段内序号（C1 D2 编号事实）：persona(0) / 07_runtime(1)
                // 由收集段持有；已迁移 gated 10=3 / 11=4 / 13=5 同由持有者
                // 声明；15_channel=6 显式声明（编号不重排）
                order: *order,
                content: SectionContent::Builtin(content),
                gate: SectionGate::Feature(*gate),
            });
        }
        // 2. 收集段落按 ID 覆盖内置（位置属性以持有者声明为准）
        for section in collected {
            let resolved = ResolvedSection {
                id: section.id,
                zone: section.zone,
                order: section.order,
                content: match &section.content {
                    PromptSectionContent::Builtin(s) => SectionContent::Builtin(s),
                    PromptSectionContent::Dynamic(s) => SectionContent::Dynamic(s.clone()),
                },
                gate: SectionGate::Always, // 收集即持有者已装配（契约 3）
            };
            match sections.iter_mut().find(|s| s.id == section.id) {
                Some(existing) => *existing = resolved,
                None => sections.push(resolved),
            }
        }
        // 3. MetaHarness 覆盖合并（覆盖 = 替换持有者对应段落贡献，覆盖优先）
        for section in &mut sections {
            if let Some(overridden) = state.section_overrides.get(section.id) {
                section.content = SectionContent::Override(Arc::clone(overridden));
            }
        }
        // 4. 空内容过滤（契约 4）+ 按"位置 + 段内序号"排序（契约 2；stable
        //    排序保持同位置同序号的声明顺序）
        sections.retain(|s| !s.content.as_str().is_empty());
        sections.sort_by_key(|s| (s.zone, s.order));

        let mut cached_sections = Vec::new();
        let mut uncached_sections = Vec::new();
        for section in sections {
            match section.zone {
                PromptSectionZone::Cached => cached_sections.push(section),
                PromptSectionZone::Uncached => uncached_sections.push(section),
            }
        }
        Self {
            cached_sections,
            uncached_sections,
            built_in_subagents_enabled: state.built_in_subagents_enabled,
        }
    }

    /// 渲染完整系统提示词
    ///
    /// 拼接顺序（固定顺序，persona 是普通收集段，不特判）：
    ///  1. 缓存区段落（zone=Cached：01-06，按段内序号）——任何 override
    ///     分支都执行；
    ///  2. 非缓存区段落（zone=Uncached：persona → 07_runtime → gated →
    ///     language，按段内序号，按 gate 判定）。
    ///
    /// 之后应用占位符替换（cwd, is_git_repo, platform, os_version, date, available_agents）。
    ///
    /// 波 4 演进（C2）：boundary 文本标记删除（设计 §3.5.2 步骤 1）——缓存
    /// 区划分由段落位置属性（zone）承担装配机制，不再生成提示词文本标记；
    /// Language 段由 LangMiddleware 持有（经 collected 注入），不再有
    /// language 参数。
    pub fn render(
        &self,
        env: &PromptEnv,
        features: &PromptFeatures,
        skills: &dyn SkillsPort,
        extra_agent_dirs: &[std::path::PathBuf],
    ) -> String {
        let mut result = String::new();

        // 1. 缓存区段落（01-06，段内序号升序）
        //    无条件渲染：`prompt_mode: full` / persona override 不得移除。
        for (i, section) in self.cached_sections.iter().enumerate() {
            if i > 0 {
                result.push_str("\n\n");
            }
            result.push_str(section.content.as_str());
        }

        // 2. 非缓存区段落（persona → 07_runtime → gated → language，按段内
        //    序号升序；gate 判定：内置 gated 段按 PromptFeatures，收集段恒渲染）
        for section in &self.uncached_sections {
            if section.gate.is_enabled(features) {
                result.push_str("\n\n");
                result.push_str(section.content.as_str());
            }
        }

        // 占位符替换（顺序与全部构造点一致）
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
                &format_available_agents(
                    skills,
                    &env.cwd,
                    extra_agent_dirs,
                    self.built_in_subagents_enabled,
                ),
            )
    }
}

impl Default for PromptTemplate {
    fn default() -> Self {
        Self::new(
            &peri_acp_types::meta_harness::MetaHarnessState::default(),
            &[],
        )
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
    include_built_ins: bool,
) -> String {
    let agents = skills.agents(cwd, extra_agent_dirs, include_built_ins);
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

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
