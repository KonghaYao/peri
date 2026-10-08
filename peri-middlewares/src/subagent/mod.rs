use peri_agent::middleware::capabilities as hook_state;
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{
    agent::{events::AgentEventHandler, AgentCancellationToken},
    error::AgentResult,
    messages::BaseMessage,
    middleware::{
        prompt_sections::{PromptSection, PromptSectionZone},
        r#trait::Middleware,
    },
    session::Session,
    tools::BaseTool,
};

use peri_acp_types::builtin_mcp::original_tool_name_of_effective;

use crate::tools::BoxToolWrapper;
use peri_acp_types::agents::AgentOverrides;
use peri_mcp_core::agent_definition::{ClaudeAgentFrontmatter, ToolsValue};

mod agent_result;
mod fork;
mod skill_preload;
mod tool;

#[cfg(test)]
pub(crate) mod test_support;
pub use agent_result::AgentResultTool;
pub use fork::{build_bg_fork_directive, build_fork_directive, build_prediction_directive};
use parking_lot::RwLock;
pub use skill_preload::SkillPreloadMiddleware;
pub use tool::SubAgentTool;
pub use tool::SubagentChainAssemblerImpl;

/// SubAgent 中间件链构造配置
///
/// 中间件链顺序固定: AgentsMd -> Skills -> [SkillPreload] -> Todo
/// 仅 `skill_names` 在不同执行路径间变化
pub struct SubAgentMiddlewareConfig {
    /// 需要预加载的 skill 名称列表，为空时跳过 SkillPreloadMiddleware
    pub skill_names: Vec<String>,
    /// 工作目录，用于解析 skill 文件路径
    pub cwd: String,
    /// Frozen CLAUDE.md/AGENTS.md main content (with @import resolved)。
    /// None 时从磁盘读取（违反 session 内 frozen 不变性，仅遗留/测试场景使用）。
    /// 由 main agent 在 session/new 时捕获并透传。
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content（与 `frozen_claude_md` 配对）。
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary。None 时从磁盘读取。
    pub frozen_skill_summary: Option<String>,
    /// 会话级 MCP skill registry（W4b：子链技能目录与正文的唯一来源；
    /// None = 未装配技能面，miss 即缺口报告，不回落磁盘）。
    pub mcp_skill_registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,
    /// 装配期关闭的 middleware 名集合（父会话冻结状态投影；
    /// 子链独立装配，必须同样过滤——设计 §2.5）。
    pub meta_harness_disabled: std::collections::HashSet<String>,
}

impl SubAgentMiddlewareConfig {
    /// Fork 路径配置（无 skill 预加载）
    pub fn for_fork(cwd: &str) -> Self {
        Self {
            skill_names: Vec::new(),
            cwd: cwd.to_string(),
            frozen_claude_md: None,
            frozen_claude_local_md: None,
            frozen_skill_summary: None,
            mcp_skill_registry: None,
            meta_harness_disabled: std::collections::HashSet::new(),
        }
    }
    /// Agent 定义路径配置
    ///
    /// `skills` 来自 `agent_def.frontmatter.skills`，为空时跳过 SkillPreloadMiddleware
    pub fn for_agent_def(skills: Vec<String>, cwd: &str) -> Self {
        Self {
            skill_names: skills,
            cwd: cwd.to_string(),
            frozen_claude_md: None,
            frozen_claude_local_md: None,
            frozen_skill_summary: None,
            mcp_skill_registry: None,
            meta_harness_disabled: std::collections::HashSet::new(),
        }
    }
    /// 注入装配期关闭的 middleware 名集合（MetaHarness，设计 §2.5）。
    ///
    /// 子链独立装配：主链关闭的 middleware（AgentsMd/Skills/SkillPreload/
    /// Todo）必须在子链同样关闭，否则子 agent 仍携带其工具与提示词贡献。
    pub fn with_meta_harness_disabled(
        mut self,
        disabled: std::collections::HashSet<String>,
    ) -> Self {
        self.meta_harness_disabled = disabled;
        self
    }
    /// 注入 main agent 在 session/new 时捕获的 frozen 数据。
    ///
    /// [TRAP] SubAgent 必须复用 main agent 的 frozen CLAUDE.md/Skills，
    /// 否则文件在会话中被修改会导致 SubAgent 与 main agent 行为漂移，
    /// 违反 "系统提示词稳定性是第一优先级" 不变量。
    pub fn with_frozen(
        mut self,
        claude_md: Option<String>,
        claude_local_md: Option<String>,
        skill_summary: Option<String>,
    ) -> Self {
        self.frozen_claude_md = claude_md;
        self.frozen_claude_local_md = claude_local_md;
        self.frozen_skill_summary = skill_summary;
        self
    }

    /// 注入会话级 MCP skill registry（W4b：子链技能的目录与正文来源）。
    pub fn with_mcp_registry(
        mut self,
        registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,
    ) -> Self {
        self.mcp_skill_registry = registry;
        self
    }
}

/// SubAgentMiddleware - injects `Agent` tool into the parent agent
///
/// In the `before_agent` phase, provides `SubAgentTool` to the parent agent via `collect_tools`,
/// enabling the LLM to call the `Agent` tool to delegate sub-tasks to specialized sub-agents.
///
/// `#[derive(Clone)]`：字段全为 Arc/Option，clone 廉价；builder 需要同时把本中间件
/// 加入 chain 与保留在 `AgentComponents.subagent_mw`（供主 v2 session 创建后注入
/// `parent_agent_id`）。
///
/// # Usage Example
///
/// ```rust,ignore
/// let parent_tools: Vec<Box<dyn BaseTool>> = vec![
///     Box::new(ReadFileTool::new(cwd)),
/// ];
/// // H1：工厂只产出模型来源；bridge（身份 + 请求时贡献）由 Agent 层子链装配点构造
/// let llm_factory = Arc::new(move |_: Option<&str>| {
///     SubagentLlmSource::model(model.clone(), "model-name")
/// });
/// // Optional: system prompt builder, making sub-agent's tone/proactiveness visible in Langfuse
/// let system_builder = Arc::new(|overrides: Option<&AgentOverrides>, cwd: &str| {
///     build_system_prompt(overrides, cwd)
/// });
/// let middleware = SubAgentMiddleware::new(parent_tools, Some(event_handler), llm_factory)
///     .with_system_builder(system_builder);
/// // 注册到 middleware chain，由 v2 stages 自动 collect_tools 收集 SubAgentTool
/// ```
#[derive(Clone)]
pub struct SubAgentMiddleware {
    /// Parent agent tool set (Arc shared, passed to child agent for use)
    parent_tools: Arc<Vec<Arc<dyn BaseTool>>>,
    /// Parent agent event handler (transparent forwarding of child agent events)
    event_handler: Option<Arc<dyn AgentEventHandler>>,
    /// 子模型工厂（H1）：只产出 [`SubagentLlmSource`]；参数是可选 model alias
    /// （"haiku"/"sonnet"/"opus"），None 表示父模型。身份 system 与请求时贡献
    /// provider 由 Agent 层 session factory 在子链装配点统一装上。
    #[allow(clippy::type_complexity)]
    llm_factory:
        Arc<dyn Fn(Option<&str>) -> peri_agent::session::subagent::SubagentLlmSource + Send + Sync>,
    /// System prompt builder: (agent overrides, cwd) -> system prompt string
    #[allow(clippy::type_complexity)]
    system_builder: Option<Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync>>,
    /// Parent agent cancellation token (passed to child agent, supports user interruption)
    cancel: Option<AgentCancellationToken>,
    /// Shared reference to parent agent message snapshot, written in before_agent, read by Fork child agent
    parent_messages: Option<Arc<RwLock<Vec<BaseMessage>>>>,
    /// Registered hooks for SubagentStart/SubagentStop lifecycle events
    registered_hooks: Arc<Vec<crate::hooks::types::RegisteredHook>>,
    /// Per-child agent event handler factory: takes agent_id → returns handler for that child.
    #[allow(clippy::type_complexity)]
    child_handler_factory: Option<Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>>,
    /// 父 agent 事件侧 AgentId 共享 cell（builder 在主 v2 session 创建后注入；
    /// SubAgentTool 与 SubAgentMiddleware 共享同一 Arc）
    parent_agent_id: Arc<RwLock<Option<peri_acp_types::identity::AgentId>>>,
    /// 父 v2 session（L3）：builder 在主 session 创建后注入；subagent 创建所需的
    /// 运行时通道（[`SubagentHost`]）与 frozen 数据经它读取，Middleware 不再
    /// 逐字段透传（L3 管理权移出）。
    parent_session: Arc<RwLock<Option<Arc<Session>>>>,
    /// 会话级 MCP Agents registry（远端定义晚读、晚批准）。
    mcp_agent_registry: Option<Arc<crate::mcp::McpAgentRegistry>>,
    /// 会话级 MCP skill registry（W4b：子链的技能目录与正文来源；None = 未装配，
    /// 子链无技能面——不回退磁盘，J5）。
    mcp_skill_registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,
    /// MCP Agent 激活使用的用户交互 broker。
    broker: Option<Arc<dyn peri_agent::interaction::UserInteractionBroker>>,
    /// 后台任务管理器是否可用（能力声明，非持有；collect_tools 时决定是否
    /// 注册 AgentResultTool）
    task_manager_available: bool,
}

impl SubAgentMiddleware {
    #[allow(clippy::type_complexity)]
    pub fn new(
        parent_tools: Vec<Box<dyn BaseTool>>,
        event_handler: Option<Arc<dyn AgentEventHandler>>,
        llm_factory: Arc<
            dyn Fn(Option<&str>) -> peri_agent::session::subagent::SubagentLlmSource + Send + Sync,
        >,
    ) -> Self {
        let tools: Vec<Arc<dyn BaseTool>> = parent_tools
            .into_iter()
            .map(|t| Arc::new(BoxToolWrapper(t)) as Arc<dyn BaseTool>)
            .collect();
        Self {
            parent_tools: Arc::new(tools),
            event_handler,
            llm_factory,
            system_builder: None,
            cancel: None,
            parent_messages: None,
            registered_hooks: Arc::new(Vec::new()),
            child_handler_factory: None,
            parent_agent_id: Arc::new(RwLock::new(None)),
            parent_session: Arc::new(RwLock::new(None)),
            mcp_agent_registry: None,
            mcp_skill_registry: None,
            broker: None,
            task_manager_available: false,
        }
    }

    pub fn with_mcp_agents(
        mut self,
        registry: Option<Arc<crate::mcp::McpAgentRegistry>>,
        broker: Arc<dyn peri_agent::interaction::UserInteractionBroker>,
    ) -> Self {
        self.mcp_agent_registry = registry;
        self.broker = Some(broker);
        self
    }

    /// 注入会话级 MCP skill registry（W4b：子链技能目录/正文的唯一来源）。
    pub fn with_mcp_skills(
        mut self,
        registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,
    ) -> Self {
        self.mcp_skill_registry = registry;
        self
    }

    /// Set system prompt builder, child agent injects system prompt via `with_system_prompt()` during execution
    #[allow(clippy::type_complexity)]
    pub fn with_system_builder(
        mut self,
        builder: Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync>,
    ) -> Self {
        self.system_builder = Some(builder);
        self
    }

    /// Set parent agent cancellation token (passed to child agent, supports user interruption of child agent execution)
    pub fn with_cancel(mut self, cancel: AgentCancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Set shared parent message reference for Fork child agent inheritance
    pub fn with_parent_messages(mut self, messages: Arc<RwLock<Vec<BaseMessage>>>) -> Self {
        self.parent_messages = Some(messages);
        self
    }

    /// Set registered hooks for SubagentStart/SubagentStop lifecycle events
    pub fn with_registered_hooks(
        mut self,
        hooks: Vec<crate::hooks::types::RegisteredHook>,
    ) -> Self {
        self.registered_hooks = Arc::new(hooks);
        self
    }

    /// Set per-child agent event handler factory.
    /// When set, `SubAgentTool::invoke` uses `factory(agent_id)` to create a dedicated
    /// event handler for each child agent, instead of wrapping the parent's shared handler.
    /// This avoids Lock contention (e.g., Langfuse Mutex) when multiple SubAgents run concurrently.
    #[allow(clippy::type_complexity)]
    pub fn with_child_handler_factory(
        mut self,
        factory: Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>,
    ) -> Self {
        self.child_handler_factory = Some(factory);
        self
    }

    /// 注入父 agent 事件侧 AgentId（主 v2 session 创建后调用）。
    /// SubAgentTool 持有同一共享 cell，invoke 时（必然晚于本调用）读到已 set 的值。
    pub fn set_parent_agent_id(&self, id: peri_acp_types::identity::AgentId) {
        *self.parent_agent_id.write() = Some(id);
    }

    /// 注入父 v2 session（L3，主 session 创建后调用）：subagent 创建所需的
    /// 运行时通道（[`SubagentHost`]）与 frozen 数据经它读取。
    pub fn set_parent_session(&self, session: Arc<Session>) {
        *self.parent_session.write() = Some(session);
    }

    /// 设置后台任务管理器可用性（assembly 注入，仅能力声明）。
    pub fn set_task_manager_available(&mut self, available: bool) {
        self.task_manager_available = available;
    }

    /// 段落声明（渲染面收集与链收集的单一事实源；C3 迁移，设计 §3.5.1
    /// 步骤 3——文件留在 `peri-acp/prompts/sections/` 由 middleware
    /// `include_str!`，内容不复制）。
    ///
    /// 11_subagent 段为 Builtin 文本，**含 `{{available_agents}}` 占位符**：
    /// catalog 替换留在渲染层（`format_available_agents`，prompt/mod.rs，
    /// 设计 §3.5.1 步骤 2——middleware 仅作内容载体，语义边界 ①）。
    ///
    /// 契约 3（gate 原子迁移）：本段 gate = 本 middleware 是否在链上
    /// （收集即装配）——关闭 SubAgentMiddleware → 11_subagent 段落 +
    /// SubAgentTool/AgentResultTool 同时消失（盲区闭合）。
    pub fn sections() -> Vec<PromptSection> {
        vec![PromptSection::builtin(
            "11_subagent",
            PromptSectionZone::Uncached,
            4, // C1 D2 编号事实：gated 11=4（10_hitl=3 之后）
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../peri-acp/prompts/sections/11_subagent.md"
            )),
        )]
    }

    /// Build SubAgentTool instance (clone Arc fields, do not transfer ownership)
    pub fn build_tool(&self, cwd: &str) -> SubAgentTool {
        let mut tool = SubAgentTool::new(
            Arc::clone(&self.parent_tools),
            self.event_handler.clone(),
            Arc::clone(&self.llm_factory),
            cwd.to_string(),
        );
        if let Some(ref builder) = self.system_builder {
            tool = tool.with_system_builder(Arc::clone(builder));
        }
        if let Some(ref cancel) = self.cancel {
            tool = tool.with_cancel(cancel.clone());
        }
        if let Some(ref pm) = self.parent_messages {
            tool = tool.with_parent_messages(Arc::clone(pm));
        }
        if !self.registered_hooks.is_empty() {
            tool = tool.with_registered_hooks(self.registered_hooks.to_vec());
        }
        if let Some(ref factory) = self.child_handler_factory {
            tool = tool.with_child_handler_factory(Arc::clone(factory));
        }
        tool = tool.with_mcp_agents(self.mcp_agent_registry.clone(), self.broker.clone());
        // W4b（F5/J5）：把会话级 MCP skill registry 交给子链装配器——子代理
        // `skills:` 预载只按名查该 registry（未命中=缺口，不回落磁盘）。
        tool = tool.with_mcp_skills(self.mcp_skill_registry.clone());
        // 共享父 agent 身份 cell（C2：Start/Stop 事件的 agent_id 字段）
        tool = tool.with_parent_agent_id(Arc::clone(&self.parent_agent_id));
        // L3：父 v2 session（运行时通道 + frozen 数据经 host 读取）
        if let Some(ref session) = *self.parent_session.read() {
            tool = tool.with_parent_session(Arc::clone(session));
        }
        tool
    }
}

/// Agent 运行时能力画像，用于主 Agent 调度决策。
///
/// 主 Agent 在 Prompt 中看到此信息后可以判断能否并行调度（readonly/writes）与
/// 期望档位；3.0 批 2 波 1 起协议类型归契约层（定义见
/// `peri_acp_types::agents::AgentCapability`）。
pub use peri_acp_types::agents::AgentCapability;

/// 按名匹配的候选集合（A4 ⑦ 匹配型归一）：原样（小写化）恒在其中，命中归一表
/// （[`original_tool_name_of_effective`]，IF-D15 唯一入口）时再补一个原始工具名候选。
///
/// 未命中（未知 / 外部 `mcp__*`）时只有一个候选 ⇒ 与迁移前的单名比较逐位一致。
/// `tools` / `disallowedTools` 声明写裸名（agent.md）而模型面工具名是 effective
/// name（builtin 一等工具），双侧展开才能命中；名字字面量只在声明表声明一份，
/// 本模块不复制、不反拆（与 `peri_agent::session::tool_catalog` 的同名函数同语义）。
pub(crate) fn name_candidates(name: &str) -> Vec<String> {
    let lowered = name.to_lowercase();
    match original_tool_name_of_effective(name) {
        Some(original) => vec![lowered, original.to_lowercase()],
        None => vec![lowered],
    }
}

/// 声明列表（`tools` / `disallowedTools`）是否覆盖名字 `name`——双侧归一候选展开。
///
/// 迁移前是两侧 `to_lowercase()` 的直接相等比较；未命中归一表的名字候选集合只有
/// 一个元素 ⇒ 未命中路径与迁移前逐位一致。`*` 通配由调用方单独处理。
pub(crate) fn declared_names_cover(declared: &[String], name: &str) -> bool {
    let candidates = name_candidates(name);
    declared
        .iter()
        .flat_map(|declared_name| name_candidates(declared_name))
        .any(|declared_candidate| candidates.contains(&declared_candidate))
}

/// 工具名是否为项目写能力（保守集合，D5）。
///
/// - 显式工具名：`Bash`（echo > file / rm / git commit）、`Write`、`Edit`、
///   `folder_operations`（含 create/delete/move 操作）、`cron_register`
///   （可定时触发任意 prompt，等价委派执行权）；
/// - 前缀：`mcp__*`（外部能力，无法静态证明只读）。
///
/// **生效名归一（IF-D6 / A4 判定型）**：builtin 一等工具的 effective name 按其
/// 原始工具名判定（例如 web 实例 `WebFetch` 的 effective name ⇒ `WebFetch`
/// ⇒ 非 mutation），否则「按名字判定」的结论会因 `mcp__` 前缀而不等于原始名。命中声明表
/// （[`original_tool_name_of_effective`]，IF-D15 唯一归一入口）才替换；未命中
/// （未知 / 外部 `mcp__*`）保持既有保守语义分毫不变。
///
/// 匹配大小写不敏感（与 `filter_tools` 一致）。
fn is_mutation_tool(name: &str) -> bool {
    let normalized = original_tool_name_of_effective(name).unwrap_or(name);
    let lower = normalized.to_lowercase();
    matches!(
        lower.as_str(),
        "bash" | "write" | "edit" | "folder_operations" | "cron_register"
    ) || lower.starts_with("mcp__")
}

/// 核心写能力工具是否被 disallowed 全部覆盖（Empty / wildcard 继承场景）。
///
/// `mcp__*` 无法用精确 disallowed 排除（`filter_tools` 为精确匹配），
/// 因此本函数只覆盖可精确排除的核心集合；这是已知局限——readonly 标签
/// 仅是调度提示，不构成安全边界，最终能力由 filter_tools 真裁剪。
///
/// disallowed 侧经 [`declared_names_cover`] 归一候选展开：写裸名
/// （`disallowedTools: [Bash, Write, …]`）与写 effective name
/// （`mcp__workspace__Bash` …）都命中 `MUTATION_CORE`。
fn core_mutation_tools_fully_disallowed(disallowed: &[String]) -> bool {
    const MUTATION_CORE: [&str; 5] = [
        "bash",
        "write",
        "edit",
        "folder_operations",
        "cron_register",
    ];
    MUTATION_CORE
        .iter()
        .all(|tool| declared_names_cover(disallowed, tool))
}

/// 从 Agent frontmatter 推断运行时能力画像（D5：保守 readonly）。
///
/// 区分三种 tools 语义（`peri_mcp_core::agent_definition::ToolsValue`）：
/// - `Empty`（字段省略）= 继承父工具（含 Bash）→ 默认 writes；
/// - `NoTools`（显式 `tools: []`）= 零工具 → readonly；
/// - `List` = 白名单，含 `*` 等价继承全部。
pub fn infer_agent_capability(fm: &ClaudeAgentFrontmatter) -> AgentCapability {
    let model_tier = fm
        .model
        .as_deref()
        .filter(|m| !m.is_empty() && *m != "inherit")
        .unwrap_or("inherit")
        .to_string();

    let disallowed = fm.disallowed_tools.to_vec();
    let can_mutate = match &fm.tools {
        ToolsValue::Empty => !core_mutation_tools_fully_disallowed(&disallowed),
        ToolsValue::NoTools => false,
        ToolsValue::List(list) if list.len() == 1 && list[0] == "*" => {
            !core_mutation_tools_fully_disallowed(&disallowed)
        }
        ToolsValue::List(tools) => tools
            .iter()
            .any(|tool| is_mutation_tool(tool) && !declared_names_cover(&disallowed, tool)),
    };

    AgentCapability {
        model_tier,
        can_mutate,
    }
}

// L5：SubAgent 中间件端口实现（stage 装配经端口注入主 agent 身份，
// 不直接引用本类型；见 peri_agent::session::factory::SubAgentMiddlewarePort）。
impl peri_agent::session::factory::SubAgentMiddlewarePort for SubAgentMiddleware {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn set_parent_agent_id(&self, id: peri_acp_types::identity::AgentId) {
        SubAgentMiddleware::set_parent_agent_id(self, id);
    }

    fn set_parent_session(&self, session: Arc<Session>) {
        SubAgentMiddleware::set_parent_session(self, session);
    }
}

#[async_trait]
impl Middleware for SubAgentMiddleware {
    fn name(&self) -> &str {
        "SubAgentMiddleware"
    }

    /// 声明持有的系统提示词段落（11_subagent，内容载体；装配期收集，契约 2）。
    fn prompt_sections(&self) -> Vec<PromptSection> {
        Self::sections()
    }

    fn collect_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>> {
        let mut tools: Vec<Box<dyn BaseTool>> = vec![Box::new(self.build_tool(cwd))];
        if self.task_manager_available {
            tools.push(Box::new(AgentResultTool::new()));
        }
        tools
    }

    async fn before_agent(&self, state: &mut dyn hook_state::BeforeAgentState) -> AgentResult<()> {
        // Snapshot current state.messages to shared reference for Fork child agent inheritance
        if let Some(ref pm) = self.parent_messages {
            *pm.write() = state.messages().to_vec();
        }
        Ok(())
    }

    async fn before_reason_catalog(
        &self,
        state: &mut dyn hook_state::CatalogState,
    ) -> AgentResult<()> {
        let Some(local_tools) = state.local_tools() else {
            return Ok(());
        };
        let Some(parent) = self.parent_session.read().clone() else {
            return Ok(());
        };
        let mut tools = local_tools.write();
        if !tools.get("Agent").is_some_and(|tool| {
            tool.mcp_server_name().is_none() && tool.namespace() == Some("interaction")
        }) {
            return Ok(());
        }
        let mut agent = self.build_tool(&parent.store().cwd);
        agent.parent_tools = Arc::new(
            self.parent_tools
                .iter()
                .filter(|tool| tool.mcp_server_name().is_none())
                .cloned()
                .chain(
                    tools
                        .values()
                        .filter(|tool| tool.mcp_server_name().is_some())
                        .filter(|tool| {
                            !matches!(
                                state.tool_source(tool.name()),
                                Some(peri_agent::session::tool_catalog::ToolSource::DynamicMcp(_))
                            )
                        })
                        .cloned(),
                )
                .collect(),
        );
        tools.insert("Agent".to_string(), Arc::new(agent));
        Ok(())
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
