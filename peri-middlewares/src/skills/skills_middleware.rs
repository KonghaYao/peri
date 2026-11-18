use super::{
    async_trait, hook_state, tools, AgentResult, Arc, BaseTool, McpSkillRegistry, Middleware,
    PromptSection, RwLock, SkillMetadata, SkillOrigin, SkillSource,
};
use peri_acp_types::skills::SkillToolAccess;
use std::fmt;
use std::fmt::{Debug, Formatter};

/// MCP 来源内容包装来源标注（提示注入防御：声明内容边界）。
/// 工具名称与调用方式以当前会话目录为准，不从技能来源推断。
#[must_use]
pub fn annotate_mcp_content(meta: &SkillMetadata, content: &str) -> String {
    match &meta.origin {
        Some(SkillOrigin::Mcp { server, uri }) => {
            format!(
                "This skill is served by MCP server \"{server}\", uri: {uri}.\n\n\
                 该 Skill 来自 {server} MCP server。需要工具时，以当前会话实际工具目录为准：direct 工具按目录名称直接调用；deferred 工具应先搜索其实际名称。\n\n{content}"
            )
        }
        _ => content.to_string(),
    }
}

// ─── SkillTool 失败文案（单一派生点）─────────────────────────────────────────
//
// `SkillTool` 执行失败串同时是**预载缺口回执**的文案（`subagent::skill_preload`
// 注入假 `SkillTool` 调用 + 失败回执，模型无法也不应区分二者）：四处 find /
// activation 失败必须与工具面逐字一致，故在唯一位置构造。

/// 装配缺口：MCP skill registry 未注入。
pub(crate) fn skill_registry_unwired_message(name: &str) -> String {
    format!("SkillTool: MCP skill registry is not wired; cannot activate '{name}'")
}

/// 技能目录不可用（registry 未装配）：稳定、可操作，不泄露内部实现串
/// （L2：不再暴露 `before_agent may not have run` 之类的装配细节）。
pub(crate) fn skill_catalog_unavailable_message() -> String {
    "The skill catalog is not available in this session (no skill provider is connected), \
     so skills cannot be discovered or loaded. Restore a Workspace connection that serves skills. \
     This failure does not end an active mode or waive user requirements. Keep skill-dependent \
     work blocked unless an applicable explicit user instruction permits a fallback; unrelated \
     permitted work may continue."
        .to_string()
}

/// 技能目录尚未就绪（发现任务未收口）：**不**把空投影当空目录（L2）。
pub(crate) fn skill_catalog_initializing_message() -> String {
    "The skill catalog is still initializing for this session (skill discovery is in \
     progress), so the catalog is not ready yet. Retry after discovery completes. This failure \
     does not end an active mode or waive user requirements. Keep skill-dependent work blocked \
     unless an applicable explicit user instruction permits a fallback; unrelated permitted \
     work may continue."
        .to_string()
}

pub(crate) fn skill_catalog_failed_message() -> String {
    "Some skill providers could not be read; the catalog may be incomplete. Reconnect the failed provider to reload the catalog; available skills remain usable. This failure does not end an active mode or waive user requirements. Keep work dependent on a missing skill blocked unless an applicable explicit user instruction permits a fallback; unrelated permitted work may continue.".to_string()
}

/// 名称未命中（含别名/全名/裸名全部形态）。
pub(crate) fn skill_not_found_message(name: &str) -> String {
    format!("Skill '{name}' not found. Use DiscoverSkillsTool to see available skills.")
}

/// 跨 origin 同名：给出候选清单（`candidate_list` 同源排序），要求完整名消歧。
pub(crate) fn skill_ambiguous_message(name: &str, candidates: &[SkillMetadata]) -> String {
    let list = crate::mcp::skill_discovery::candidate_list(candidates);
    format!(
        "Skill '{name}' is ambiguous across {} origins: {list}. \
         Use the full name ('<server>:<skill>' or 'mcp__<server>__<skill>') to disambiguate.",
        candidates.len()
    )
}

/// 命中但激活失败（digest/frontmatter/内容绑定校验不通过）。
pub(crate) fn skill_activation_failed_message(name: &str, reason: &str) -> String {
    format!("SkillTool: cannot activate '{name}' ({reason})")
}

/// 预载预算（条数 / 单项 / 整批字节）挡下：**不加载截断正文冒充完整指令**，
/// 改为可操作的缺口回执——模型可以按需用 `SkillTool` 重新加载。
pub(crate) fn skill_preload_budget_message(name: &str, reason: &str) -> String {
    format!(
        "SkillTool: '{name}' was not preloaded ({reason}). \
         Call SkillTool('{name}') to load it on demand."
    )
}

/// SkillsMiddleware — 渐进式 Skills 摘要注入（J5：零文件系统依赖）。
///
/// 数据源只有一个：会话级 [`McpSkillRegistry`]（workspace 实例承担本地三根 /
/// 插件根 / builtin 静态资产；外部 MCP server 原样）。本中间件**不做**任何
/// 扫描、不读盘：`before_agent` 只把 registry 的当前投影复制进 `cached_skills`
/// （工具面共享），并按投递规则生成 prompt contribution。
///
/// 投递规则（J1 + §5.4）：
/// - 冻结摘要存在（session/new 由 system 来源生成）→ 会话内固定复用该文本；
/// - 未冻结（legacy/无 MCP 面）→ 只用**系统来源**（`ConfigSource::Builtin`，
///   见 `McpSkillRegistry::mark_system_origins`）的当前投影渲染；非 system 来源
///   保持既有延迟发现语义，不自动进 prompt。
pub struct SkillsMiddleware {
    /// Frozen skills summary (None = 每轮按系统来源投影渲染)。
    pub(super) frozen_summary: Option<String>,
    /// Cached prompt contribution (populated in before_agent, returned by prompt_contribution).
    pub(super) cached_contribution: Arc<RwLock<Option<String>>>,
    prompt_access: RwLock<SkillToolAccess>,
    register_tools: bool,
    /// MCP 远端技能注册表（None = 未装配技能面：投影为空，不回退任何本地来源）。
    pub(super) mcp_registry: Option<Arc<McpSkillRegistry>>,
}

impl Debug for SkillsMiddleware {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SkillsMiddleware")
            .field("frozen_summary", &self.frozen_summary)
            .finish_non_exhaustive()
    }
}

// ─── 13_skills 段落持有（波 4 演进 C3，设计 §3.1.1 归属全景 / §3.1.2）───────

/// discovery 协议 markdown 文本（13_skills 段落动态部分）。
///
/// W4b（J5）后的代码事实：技能目录由 **MCP 侧**提供——builtin `workspace`
/// 实例按宿主装配的资源根（user / project / plugin / builtin 静态资产）
/// 经 `skills/list` 提供 manifest、经 `resources/read` 提供正文；外部 MCP server
/// 经同一通道提供。宿主侧不再有磁盘扫描路径，因此本段只描述发现/加载协议，
/// 不再声明任何本地路径或扫描深度。
#[must_use]
pub fn format_discovery_protocol() -> String {
    [
        "Skill catalog is served by MCP servers; the builtin `workspace` instance serves \
         this machine's skill roots (user / project `.claude/skills` / \
         plugin manifests / builtin assets). No local path is read by the agent.",
        "Use `DiscoverSkillsTool` to list the catalog (name + source) and `SkillTool(skill_name)` \
         to load a skill's full text by name. Cross-origin name collisions are rejected as \
         ambiguous — disambiguate with `<server>:<skill>` or `mcp__<server>__<skill>`.",
        "Skills reach the model as metadata only (name + source); full text is loaded on demand.",
    ]
    .join("\n")
}

impl SkillsMiddleware {
    /// 段落声明（渲染面收集与链收集的单一事实源；C3 迁移，设计 §3.1.1）。
    ///
    /// 13_skills 段 = 机制说明（`sections/13_skills.md`，include_str 零拷贝，
    /// 文件在本 crate 的 `prompts/sections/`）+ 动态 discovery 协议
    /// （[`format_discovery_protocol`]）。
    ///
    /// 契约 3（gate 原子迁移）：本段 gate = 本 middleware 是否在链上
    /// （收集即装配）——关闭 SkillsMiddleware → 13_skills 段落 +
    /// SkillTool/DiscoverSkillsTool 同时消失（盲区闭合）。
    #[must_use]
    pub fn sections() -> Vec<PromptSection> {
        Self::sections_for_tools(SkillToolAccess::Full)
    }

    pub fn sections_for_tools(access: SkillToolAccess) -> Vec<PromptSection> {
        let partial = match access {
            SkillToolAccess::None => return Vec::new(),
            SkillToolAccess::LoadOnly => Some(
                "# Skills\n\nUse `SkillTool(skill_name)` to load the full skill by name. Use the exact name from the session-start metadata, user instructions, or an earlier result. After compaction, use the exact names in Recovery resources to reload any archived active skill. If loading fails, keep skill-dependent work blocked unless the user permits a fallback.",
            ),
            SkillToolAccess::DiscoverOnly => Some(
                "# Skills\n\nUse `DiscoverSkillsTool(query?)` to find current skills by name or description. The results are catalog metadata and do not include the skill body. Apply skill instructions that the harness has already loaded. If an active skill body is unavailable, keep dependent work blocked unless the user permits a fallback.",
            ),
            SkillToolAccess::Full => None,
        };
        if let Some(content) = partial {
            return vec![PromptSection::declared(
                peri_acp_types::meta_harness::sections::SKILLS,
                peri_agent::middleware::PromptSectionContent::Builtin(content),
            )
            .without_override()];
        }

        let mut content = String::with_capacity(1024);
        content.push_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/prompts/sections/13_skills.md"
        )));
        content.push_str("\n\n");
        content.push_str(&format_discovery_protocol());
        vec![PromptSection::declared(
            peri_acp_types::meta_harness::sections::SKILLS,
            peri_agent::middleware::PromptSectionContent::Dynamic(content),
        )]
    }

    #[must_use]
    pub fn new() -> Self {
        Self {
            frozen_summary: None,
            cached_contribution: Arc::new(RwLock::new(None)),
            prompt_access: RwLock::new(SkillToolAccess::Full),
            register_tools: true,
            mcp_registry: None,
        }
    }

    /// Project the child policy against the same tools the child chain contributes.
    pub fn access_for_filter(filter: impl Fn(&dyn BaseTool) -> bool) -> SkillToolAccess {
        let tools = Self::new().collect_tools("");
        SkillToolAccess::from_tools(tools.iter().map(Box::as_ref).filter(|tool| filter(*tool)))
    }

    /// 冻结身份可能来自 Windows 检出/构建（CRLF）；匹配按 LF 归一化进行，
    /// `offsets` 以归一化字节下标映射回原串字节下标。
    fn line_ending_normalized(identity: &str) -> (String, Vec<usize>) {
        let mut normalized = String::with_capacity(identity.len());
        let mut offsets = Vec::with_capacity(identity.len() + 1);
        let bytes = identity.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                offsets.push(index);
                normalized.push('\n');
                index += 2;
            } else {
                let ch = identity[index..]
                    .chars()
                    .next()
                    .expect("offset follows a char boundary");
                offsets.extend(std::iter::repeat_n(index, ch.len_utf8()));
                normalized.push(ch);
                index += ch.len_utf8();
            }
        }
        offsets.push(identity.len());
        (normalized, offsets)
    }

    pub(crate) fn project_resumed_identity(
        identity: &str,
        recorded: Option<SkillToolAccess>,
        frozen: Option<&peri_acp_types::meta_harness::MetaHarnessState>,
        requested: SkillToolAccess,
        rendered_section: Option<&str>,
    ) -> Result<(String, SkillToolAccess), String> {
        use peri_acp_types::meta_harness::{sections, AssemblyPlan, ChainSlot, ExecutionProfile};
        use sha2::{Digest, Sha256};
        use SkillToolAccess::{DiscoverOnly, Full, LoadOnly};

        // Rendered default owner section from release commit 25c183e.
        const LEGACY_LEN: usize = 5124;
        const LEGACY_DIGEST: [u8; 32] = [
            0x2b, 0xd9, 0x12, 0xf1, 0xb0, 0x88, 0xd2, 0x43, 0x38, 0xb6, 0x4d, 0x0c, 0x66, 0x33,
            0xc4, 0xc8, 0x7a, 0xe6, 0xb6, 0x77, 0x79, 0x7f, 0xf6, 0xd5, 0x10, 0x64, 0x73, 0x9c,
            0x82, 0xd7, 0x57, 0xe7,
        ];

        if frozen.is_some_and(|state| {
            !AssemblyPlan::new(&state.disabled_middlewares, ExecutionProfile::Child)
                .declares_sections(ChainSlot::Skills)
        }) {
            return Ok((identity.to_owned(), SkillToolAccess::None));
        }

        let variants =
            [Full, LoadOnly, DiscoverOnly].map(|access| (access, Self::sections_for_tools(access)));
        let overridden = frozen.and_then(|state| state.section_overrides.get(sections::SKILLS.id));
        let (normalized, offsets) = Self::line_ending_normalized(identity);
        let is_section = |start: usize, end: usize| {
            (start == 0 || normalized[..start].ends_with("\n\n"))
                && (end == normalized.len() || normalized[end..].starts_with("\n\n"))
        };
        let bodies = variants
            .iter()
            .flat_map(|(access, sections)| {
                sections
                    .iter()
                    .map(move |section| (*access, section.content.as_str()))
            })
            .chain(
                rendered_section
                    .or_else(|| overridden.map(AsRef::as_ref))
                    .map(|body| (Full, body)),
            )
            .filter(|(_, body)| !body.is_empty())
            // 段落正文同样按 LF 归一化（Windows 构建的 include_str! 产物为 CRLF）。
            .map(|(access, body)| (access, body.replace("\r\n", "\n")))
            .collect::<Vec<_>>();
        let mut matches = bodies
            .iter()
            .flat_map(|(access, body)| {
                normalized
                    .match_indices(body.as_str())
                    .filter_map(move |(start, _)| {
                        let end = start + body.len();
                        is_section(start, end).then_some((start, end, *access))
                    })
            })
            .collect::<Vec<_>>();
        for (start, _) in normalized.match_indices("# Skills\n") {
            let end = start + LEGACY_LEN;
            if let Some(body) = normalized
                .get(start..end)
                .filter(|_| is_section(start, end))
            {
                if Sha256::digest(body.as_bytes())[..] == LEGACY_DIGEST {
                    matches.push((start, end, SkillToolAccess::None));
                }
            }
        }
        matches.sort_unstable_by_key(|(start, end, _)| (*start, *end));
        matches.dedup_by(|next, previous| {
            if (next.0, next.1) != (previous.0, previous.1) {
                return false;
            }
            previous.2 = previous.2.intersect(next.2);
            true
        });
        let (start, end, allowed) = match matches.as_slice() {
            [(start, end, allowed)] => (*start, *end, *allowed),
            [] if !normalized.contains("# Skills")
                && (recorded == Some(SkillToolAccess::None)
                    || (recorded.is_none()
                        && overridden.is_none_or(|body| !body.contains("{{")))) =>
            {
                return Ok((identity.to_owned(), SkillToolAccess::None));
            }
            _ => return Err("cannot identify the frozen Skills section for resume".into()),
        };
        // 归一化偏移回到原串，保留未命中部分的原始行尾。
        let (start, end) = (offsets[start], offsets[end]);
        let ceiling = recorded.unwrap_or(SkillToolAccess::None);
        let access = requested.intersect(allowed).intersect(ceiling);
        if recorded.is_none() || allowed == SkillToolAccess::None {
            tracing::warn!(
                "Legacy child identity has no recorded skills authorization; resuming without skill tools"
            );
        } else if access == allowed {
            return Ok((identity.to_owned(), access));
        }

        let before = &identity[..start];
        let after = &identity[end..];
        let replacement = Self::sections_for_tools(access);
        let projected = if let Some(section) = replacement.first() {
            format!("{before}{}{after}", section.content.as_str())
        } else if let Some(after) = after
            .strip_prefix("\n\n")
            .or_else(|| after.strip_prefix("\r\n\r\n"))
        {
            format!("{before}{after}")
        } else {
            before
                .strip_suffix("\n\n")
                .or_else(|| before.strip_suffix("\r\n\r\n"))
                .unwrap_or(before)
                .to_owned()
        };
        Ok((projected, access))
    }

    pub(crate) fn without_tool_registration(mut self) -> Self {
        self.register_tools = false;
        self
    }

    fn update_prompt_access(&self, state: &dyn hook_state::CatalogState) {
        if let Some(tools) = state.local_tools() {
            let tools = tools.read();
            *self
                .prompt_access
                .write()
                .expect("skill prompt access lock poisoned") =
                SkillToolAccess::from_tools(tools.values().map(Arc::as_ref));
        }
    }

    /// 注入 MCP 远端技能注册表（None = 未装配技能面；默认 None，
    /// `new()` 签名与既有测试/构造点不变）。
    #[must_use]
    pub fn with_mcp_registry(mut self, reg: Option<Arc<McpSkillRegistry>>) -> Self {
        self.mcp_registry = reg;
        self
    }

    /// 注入 session/new 时冻结的 skills 摘要（system 来源的元数据快照渲染）。
    /// 设置后 summary contribution 在会话内保持稳定（ARC-FROZEN-001 的投递面）。
    ///
    /// 只影响 prompt 贡献；工具面（`SkillTool` / `DiscoverSkillsTool`）在调用时
    /// 直接读 registry 的当前投影，不经过任何中间缓存。
    #[must_use]
    pub fn with_frozen_summary(mut self, summary: String) -> Self {
        self.frozen_summary = Some(summary.clone());
        if !summary.trim().is_empty() {
            *self
                .cached_contribution
                .write()
                .expect("frozen prompt contribution must not expose an interrupted cache update") =
                Some(summary);
        }
        self
    }

    /// skill 来源标签（build_summary / DiscoverSkillsTool 共用）。
    ///
    /// MCP 条目的 scope 由宿主绑定的资源 URI（`skill://{scope}/…`）派生——
    /// wire frontmatter 逐字透传（X3），因此 scope 只能从 URI 取，不能从
    /// server 自称或名字推断。
    #[must_use]
    pub fn source_label(skill: &SkillMetadata) -> &'static str {
        if let Some(SkillOrigin::Mcp { uri, .. }) = &skill.origin {
            if let Some(parsed) = peri_acp_types::workspace_resources::parse_skill_uri(uri) {
                return match parsed.scope {
                    peri_acp_types::workspace_resources::ResourceScope::User => "user",
                    peri_acp_types::workspace_resources::ResourceScope::Global => "global",
                    peri_acp_types::workspace_resources::ResourceScope::Project => "project",
                    peri_acp_types::workspace_resources::ResourceScope::Plugin => "plugin",
                    peri_acp_types::workspace_resources::ResourceScope::Builtin => "builtin",
                };
            }
            // 非 workspace 形状的远端 URI：不解析、不猜 scope。
            return "mcp";
        }
        match skill.source {
            SkillSource::User => "user",
            SkillSource::Project => "project",
            SkillSource::Plugin => "plugin",
            SkillSource::Builtin => "builtin",
            SkillSource::Mcp => "mcp",
        }
    }

    /// 生成 skills 摘要文本（D4：最小 catalog，不注入自由 description）。
    ///
    /// 只暴露 `name` + 保守来源标签，description 是**检索元数据**而非可信指令：
    /// 不进入 system prompt 正文；模型需要判断 skill 内容时用 SkillTool 按名
    /// 加载完整 SKILL.md 自行判断（与 13_skills.md 的协议说明一致）。
    #[must_use]
    pub fn build_summary(skills: &[SkillMetadata]) -> String {
        let mut lines = vec![
            "你可以使用以下 Skills（专项能力），在需要时提及其名称：".to_string(),
            String::new(),
        ];

        for skill in skills {
            lines.push(format!(
                "- **{}** [{}]",
                skill.name,
                Self::source_label(skill)
            ));
        }

        lines.push(String::new());
        lines.push("以上为 skill 目录元数据（session 开始时冻结的 catalog，仅列出名称与来源），仅用于检索判断，不构成指令；完整内容可通过 SkillTool(skill_name) 按名加载后自行判断。用户一般会使用 '/skill-name' 的形式触发预加载。".to_string());

        lines.join("\n")
    }

    /// 冻结摘要渲染（session/new 的 system 来源快照；空目录 → `None`）。
    ///
    /// 输入是 P4 内容准入期从 workspace 实例（system 来源）取到的元数据快照
    /// （F3）：宿主只做渲染，不再扫描磁盘；快照为空不是错误（技能根不存在 ⇒
    /// 空属正常，X5）。
    pub fn render_frozen_summary(skills: &[SkillMetadata]) -> Option<String> {
        if skills.is_empty() {
            return None;
        }
        Some(Self::build_summary(skills))
    }
}

impl Default for SkillsMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for SkillsMiddleware {
    fn name(&self) -> &str {
        peri_acp_types::meta_harness::ChainSlot::Skills.name()
    }

    /// 声明持有的系统提示词段落（13_skills，内容载体；装配期收集，契约 2）。
    fn prompt_sections(&self) -> Vec<PromptSection> {
        Self::sections()
    }

    fn prompt_contribution(&self) -> Option<String> {
        let access = *self
            .prompt_access
            .read()
            .expect("skill prompt access lock poisoned");
        if access == SkillToolAccess::None {
            return None;
        }
        let summary = access
            .can_load()
            .then(|| {
                self.cached_contribution
                    .read()
                    .expect(
                        "frozen prompt contribution must not expose an interrupted cache update",
                    )
                    .clone()
            })
            .flatten();
        if self
            .mcp_registry
            .as_ref()
            .is_some_and(|registry| registry.discovery_failed())
        {
            return Some(match summary {
                Some(summary) => format!("{summary}\n\n{}", skill_catalog_failed_message()),
                None => skill_catalog_failed_message(),
            });
        }
        summary
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        if !self.register_tools {
            return Vec::new();
        }
        vec![
            Box::new(tools::SkillTool::new(self.mcp_registry.clone())),
            Box::new(tools::DiscoverSkillsTool::new(self.mcp_registry.clone())),
        ]
    }

    async fn before_reason_catalog(
        &self,
        state: &mut dyn hook_state::CatalogState,
    ) -> AgentResult<()> {
        self.update_prompt_access(state);
        Ok(())
    }

    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        self.update_prompt_access(state);
        // 投递面（J1）：冻结摘要优先（会话内不变）；未冻结时只用系统来源的当前
        // 投影渲染——非 system 来源保持既有延迟发现语义，不自动改 prompt。
        //
        // M8/L2：工具面不再消费 before_agent 时刻的目录副本——`SkillTool` /
        // `DiscoverSkillsTool` 在调用时读取 registry 的当前投影，因此这里只维护
        // prompt 贡献（「空投影 → None」的歧义缓存一并删除）。
        if let Some(ref summary) = self.frozen_summary {
            *self
                .cached_contribution
                .write()
                .expect("frozen prompt contribution must not expose an interrupted cache update") =
                if summary.trim().is_empty() {
                    None
                } else {
                    Some(summary.clone())
                };
            return Ok(());
        }
        let system_skills = self
            .mcp_registry
            .as_ref()
            .map(|registry| registry.system_skills())
            .unwrap_or_default();
        *self
            .cached_contribution
            .write()
            .expect("frozen prompt contribution must not expose an interrupted cache update") =
            Self::render_frozen_summary(&system_skills);
        Ok(())
    }
}
