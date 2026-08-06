use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use peri_agent::{
    agent::{events::AgentEventHandler, react::ReactLLM, AgentCancellationToken},
    error::AgentResult,
    messages::BaseMessage,
    middleware::{r#trait::Middleware, state::MiddlewareState},
    session::Session,
    tools::BaseTool,
};

use crate::{
    agent_define::AgentOverrides, claude_agent_parser::ClaudeAgentFrontmatter,
    claude_agent_parser::ToolsValue, parse_agent_file, tools::BoxToolWrapper,
};

mod agent_result;
mod built_in_agents;
mod fork;
mod skill_preload;
mod tool;
pub use agent_result::AgentResultTool;
pub use built_in_agents::{
    built_in_agent_types, get_built_in_agent, list_built_in_agents, BuiltInAgent,
};
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
        }
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
/// let llm_factory = Arc::new(move |_: Option<&str>| {
///     Box::new(AgentModelBridge::new(model.clone())) as Box<dyn ReactLLM + Send + Sync>
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
    /// LLM factory function, creates independent LLM instance for each child agent
    /// Parameter is optional model alias (e.g., "haiku"/"sonnet"/"opus"), None means use parent model
    #[allow(clippy::type_complexity)]
    llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
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
    /// 后台任务管理器是否可用（能力声明，非持有；collect_tools 时决定是否
    /// 注册 AgentResultTool）
    task_manager_available: bool,
}

impl SubAgentMiddleware {
    #[allow(clippy::type_complexity)]
    pub fn new(
        parent_tools: Vec<Box<dyn BaseTool>>,
        event_handler: Option<Arc<dyn AgentEventHandler>>,
        llm_factory: Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
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
            task_manager_available: false,
        }
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
        // 共享父 agent 身份 cell（C2：Start/Stop 事件的 agent_id 字段）
        tool = tool.with_parent_agent_id(Arc::clone(&self.parent_agent_id));
        // L3：父 v2 session（运行时通道 + frozen 数据经 host 读取）
        if let Some(ref session) = *self.parent_session.read() {
            tool = tool.with_parent_session(Arc::clone(session));
        }
        tool
    }
}

/// Scan `{cwd}/.claude/agents/` directory, return `(agent_id, name, description)` list.
/// Built-in agents are included as fallback — project-level agents with the same ID take precedence.
pub fn scan_agents(cwd: &str) -> Vec<(String, String, String)> {
    let mut result = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    // 1. Scan project-level agents (highest priority)
    let agents_dir = Path::new(cwd).join(".claude").join("agents");
    if agents_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&agents_dir) {
            for entry in entries.flatten() {
                let path = entry.path();

                let (agent_id, file_path): (String, PathBuf) = if path.is_file() {
                    if path.extension().and_then(|e| e.to_str()) != Some("md") {
                        continue;
                    }
                    let id = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    (id, path)
                } else if path.is_dir() {
                    let nested = path.join("agent.md");
                    if !nested.is_file() {
                        continue;
                    }
                    let id = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    (id, nested)
                } else {
                    continue;
                };

                let content = match std::fs::read_to_string(&file_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                if let Some(agent) = parse_agent_file(&content) {
                    let name = if agent.frontmatter.name.is_empty() {
                        agent_id.clone()
                    } else {
                        agent.frontmatter.name.clone()
                    };
                    let description = agent.frontmatter.description.clone();
                    seen_ids.insert(agent_id.clone());
                    result.push((agent_id, name, description));
                }
            }
        }
    }

    // 2. Append built-in agents (project-level agents take precedence by ID)
    for built_in in list_built_in_agents() {
        if seen_ids.insert(built_in.agent_id.to_string()) {
            if let Some(agent) = parse_agent_file(built_in.content) {
                let name = if agent.frontmatter.name.is_empty() {
                    built_in.agent_id.to_string()
                } else {
                    agent.frontmatter.name.clone()
                };
                let description = agent.frontmatter.description.clone();
                result.push((built_in.agent_id.to_string(), name, description));
            }
        }
    }

    // Sort by agent_id for stable output
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// 扫描 agent 目录，支持额外的插件 agent 搜索路径
/// 项目级 agent 优先，同名 agent_id 去重时保留先出现的
pub fn scan_agents_with_extra_dirs(
    cwd: &str,
    extra_dirs: &[PathBuf],
) -> Vec<(String, String, String)> {
    let mut result = scan_agents(cwd);
    let mut seen_ids: std::collections::HashSet<String> =
        result.iter().map(|(id, _, _)| id.clone()).collect();

    for dir in extra_dirs {
        if !dir.is_dir() {
            continue;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();

            let (agent_id, file_path): (String, PathBuf) = if path.is_file() {
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let id = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                (id, path)
            } else if path.is_dir() {
                let nested = path.join("agent.md");
                if !nested.is_file() {
                    continue;
                }
                let id = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                (id, nested)
            } else {
                continue;
            };

            // Skip duplicates (CWD + built-in agents already registered)
            if !seen_ids.insert(agent_id.clone()) {
                continue;
            }

            let content = match std::fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if let Some(agent) = parse_agent_file(&content) {
                let name = if agent.frontmatter.name.is_empty() {
                    agent_id.clone()
                } else {
                    agent.frontmatter.name.clone()
                };
                let description = agent.frontmatter.description.clone();
                result.push((agent_id, name, description));
            }
        }
    }

    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Agent 运行时能力画像，用于主 Agent 调度决策。
///
/// 主 Agent 在 Prompt 中看到此信息后可以判断：
// 3.0 批 2 波 1：协议类型归契约层（定义见 `peri_acp_types::agents::AgentCapability`）。
pub use peri_acp_types::agents::AgentCapability;

/// 工具名是否为项目写能力（保守集合，D5）。
///
/// - 显式工具名：`Bash`（echo > file / rm / git commit）、`Write`、`Edit`、
///   `folder_operations`（含 create/delete/move 操作）、`cron_register`
///   （可定时触发任意 prompt，等价委派执行权）；
/// - 前缀：`mcp__*`（外部能力，无法静态证明只读）。
///
/// 匹配大小写不敏感（与 `filter_tools` 一致）。
fn is_mutation_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
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
fn core_mutation_tools_fully_disallowed(disallowed: &[String]) -> bool {
    const MUTATION_CORE: [&str; 5] = [
        "bash",
        "write",
        "edit",
        "folder_operations",
        "cron_register",
    ];
    let dis_lower: Vec<String> = disallowed.iter().map(|s| s.to_lowercase()).collect();
    MUTATION_CORE
        .iter()
        .all(|t| dis_lower.iter().any(|d| d == t))
}

/// 从 Agent frontmatter 推断运行时能力画像（D5：保守 readonly）。
///
/// 区分三种 tools 语义（`claude_agent_parser::ToolsValue`）：
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
        ToolsValue::List(tools) => {
            let dis_lower: Vec<String> = disallowed.iter().map(|s| s.to_lowercase()).collect();
            tools
                .iter()
                .any(|t| is_mutation_tool(t) && !dis_lower.iter().any(|d| d == &t.to_lowercase()))
        }
    };

    AgentCapability {
        model_tier,
        can_mutate,
    }
}

/// 扫描 agent 目录并返回完整信息（含能力画像）。
///
/// 项目级 agent 优先，同名 agent_id 去重。返回 `(agent_id, name, description, capability)`。
pub fn scan_agents_detailed(
    cwd: &str,
    extra_dirs: &[PathBuf],
) -> Vec<(String, String, String, AgentCapability)> {
    let mut result = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    // 辅助闭包：扫描单个目录
    let scan_dir =
        |dir: &Path, result: &mut Vec<_>, seen_ids: &mut std::collections::HashSet<_>| {
            if !dir.is_dir() {
                return;
            }
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => return,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let (agent_id, file_path): (String, PathBuf) = if path.is_file() {
                    if path.extension().and_then(|e| e.to_str()) != Some("md") {
                        continue;
                    }
                    let id = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    (id, path)
                } else if path.is_dir() {
                    let nested = path.join("agent.md");
                    if !nested.is_file() {
                        continue;
                    }
                    let id = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    (id, nested)
                } else {
                    continue;
                };
                if !seen_ids.insert(agent_id.clone()) {
                    continue;
                }
                let content = match std::fs::read_to_string(&file_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if let Some(agent) = parse_agent_file(&content) {
                    let name = if agent.frontmatter.name.is_empty() {
                        agent_id.clone()
                    } else {
                        agent.frontmatter.name.clone()
                    };
                    let desc = agent.frontmatter.description.clone();
                    let cap = infer_agent_capability(&agent.frontmatter);
                    result.push((agent_id, name, desc, cap));
                }
            }
        };

    // 1. 项目级 agent（最高优先级，先添加则占住 seen_ids）
    let agents_dir = Path::new(cwd).join(".claude").join("agents");
    scan_dir(&agents_dir, &mut result, &mut seen_ids);

    // 2. 内置 agent（IFF 同 ID 未被项目级覆盖）
    for built_in in list_built_in_agents() {
        if seen_ids.insert(built_in.agent_id.to_string()) {
            if let Some(agent) = parse_agent_file(built_in.content) {
                let name = if agent.frontmatter.name.is_empty() {
                    built_in.agent_id.to_string()
                } else {
                    agent.frontmatter.name.clone()
                };
                let desc = agent.frontmatter.description.clone();
                let cap = infer_agent_capability(&agent.frontmatter);
                result.push((built_in.agent_id.to_string(), name, desc, cap));
            }
        }
    }

    // 3. 插件 agent（最低优先级）
    for dir in extra_dirs {
        scan_dir(dir, &mut result, &mut seen_ids);
    }

    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
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

    fn collect_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>> {
        let mut tools: Vec<Box<dyn BaseTool>> = vec![Box::new(self.build_tool(cwd))];
        if self.task_manager_available {
            tools.push(Box::new(AgentResultTool::new()));
        }
        tools
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        // Snapshot current state.messages to shared reference for Fork child agent inheritance
        if let Some(ref pm) = self.parent_messages {
            *pm.write() = state.messages().to_vec();
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
