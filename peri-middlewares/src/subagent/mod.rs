mod agent_result;
mod background;
mod built_in_agents;
mod fork;
mod skill_preload;
pub mod spawner;
mod tool;
pub use agent_result::AgentResultTool;
pub use background::{BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus};
pub use built_in_agents::{get_built_in_agent, list_built_in_agents, BuiltInAgent};
pub use fork::build_prediction_directive;
use parking_lot::RwLock;
pub use skill_preload::SkillPreloadMiddleware;
pub use spawner::{spawn_background_fork, BgForkConfig, BgForkDirectiveKind, BgForkSpawned};
pub use tool::SubAgentTool;

/// SubAgent 中间件链构造配置
///
/// 中间件链顺序固定: AgentsMd -> Skills -> [SkillPreload] -> Todo
/// 仅 `skill_names` 在不同执行路径间变化
pub(crate) struct SubAgentMiddlewareConfig {
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
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use peri_agent::thread::ThreadStore;
use peri_agent::{
    agent::{events::AgentEventHandler, react::ReactLLM, state::State, AgentCancellationToken},
    error::AgentResult,
    messages::BaseMessage,
    middleware::r#trait::Middleware,
    tools::BaseTool,
};

use crate::{
    agent_define::AgentOverrides, claude_agent_parser::ClaudeAgentFrontmatter, parse_agent_file,
    tools::BoxToolWrapper,
};

/// SubAgentMiddleware - injects `Agent` tool into the parent agent
///
/// In the `before_agent` phase, provides `SubAgentTool` to the parent agent via `collect_tools`,
/// enabling the LLM to call the `Agent` tool to delegate sub-tasks to specialized sub-agents.
///
/// # Usage Example
///
/// ```rust,ignore
/// let parent_tools: Vec<Box<dyn BaseTool>> = vec![
///     Box::new(ReadFileTool::new(cwd)),
/// ];
/// let llm_factory = Arc::new(move |_: Option<&str>| {
///     Box::new(BaseModelReactLLM::new(model.clone())) as Box<dyn ReactLLM + Send + Sync>
/// });
/// // Optional: system prompt builder, making sub-agent's tone/proactiveness visible in Langfuse
/// let system_builder = Arc::new(|overrides: Option<&AgentOverrides>, cwd: &str| {
///     build_system_prompt(overrides, cwd)
/// });
/// let middleware = SubAgentMiddleware::new(parent_tools, Some(event_handler), llm_factory)
///     .with_system_builder(system_builder);
/// let agent = ReActAgent::new(llm).add_middleware(Box::new(middleware));
/// ```
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
    /// When set, child agent injects system prompt via with_system_prompt() (visible in Langfuse)
    #[allow(clippy::type_complexity)]
    system_builder: Option<Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync>>,
    /// Parent agent cancellation token (passed to child agent, supports user interruption)
    cancel: Option<AgentCancellationToken>,
    /// Shared reference to parent agent message snapshot, written in before_agent, read by Fork child agent
    parent_messages: Option<Arc<RwLock<Vec<BaseMessage>>>>,
    /// 后台任务注册中心（通过 build_tool 传递给 SubAgentTool）
    background_registry: Option<Arc<BackgroundTaskRegistry>>,
    /// Registered hooks for SubagentStart/SubagentStop lifecycle events
    registered_hooks: Arc<Vec<crate::hooks::types::RegisteredHook>>,
    /// Per-child agent event handler factory: takes agent_id → returns handler for that child.
    /// When set, child agents use this factory instead of wrapping the parent's event_handler,
    /// avoiding shared Lock (e.g., Langfuse Mutex) contention in concurrent execution.
    #[allow(clippy::type_complexity)]
    child_handler_factory: Option<Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>>,
    /// 后台任务完成事件的���立发送通道（不随 executor 生命周期销毁）
    bg_event_sender:
        Option<tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::AgentEvent>>,
    /// Thread persistence store for child threads
    thread_store: Option<Arc<dyn ThreadStore>>,
    /// Parent thread ID for child thread hierarchy
    parent_thread_id: Option<String>,
    /// Register callback: (thread_id, cancel_token, cancel_policy_str) → inserts into active_agents map
    #[allow(clippy::type_complexity)]
    register_runtime: Option<Arc<dyn Fn(String, AgentCancellationToken, String) + Send + Sync>>,
    /// Deregister callback: removes from active_agents map by thread_id
    deregister_runtime: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Frozen CLAUDE.md/AGENTS.md main content（session/new 时捕获，Arc 共享）。
    /// 透传到 SubAgentMiddleware，确保 SubAgent 与 main agent 看到一致的 CLAUDE.md。
    frozen_claude_md: Option<Arc<String>>,
    /// Frozen CLAUDE.local.md content
    frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen skills summary
    frozen_skill_summary: Option<Arc<String>>,
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
            background_registry: None,
            registered_hooks: Arc::new(Vec::new()),
            child_handler_factory: None,
            bg_event_sender: None,
            thread_store: None,
            parent_thread_id: None,
            register_runtime: None,
            deregister_runtime: None,
            frozen_claude_md: None,
            frozen_claude_local_md: None,
            frozen_skill_summary: None,
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

    /// Set background task registry for run_in_background mode
    pub fn with_background_registry(mut self, registry: Arc<BackgroundTaskRegistry>) -> Self {
        self.background_registry = Some(registry);
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

    /// Set background task event sender.
    /// The sender survives executor lifecycle, allowing bg task results to reach TUI
    /// even after the main agent finishes.
    pub fn with_bg_event_sender(
        mut self,
        sender: tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::AgentEvent>,
    ) -> Self {
        self.bg_event_sender = Some(sender);
        self
    }

    /// Set thread persistence store for child thread creation
    pub fn with_thread_store(mut self, store: Arc<dyn ThreadStore>) -> Self {
        self.thread_store = Some(store);
        self
    }

    /// Set parent thread ID for child thread hierarchy
    pub fn with_parent_thread_id(mut self, id: String) -> Self {
        self.parent_thread_id = Some(id);
        self
    }

    /// Set register callback: called when a child agent thread starts executing.
    /// Parameters: (thread_id, cancel_token, cancel_policy_str)
    #[allow(clippy::type_complexity)]
    pub fn with_register_runtime(
        mut self,
        cb: Arc<dyn Fn(String, AgentCancellationToken, String) + Send + Sync>,
    ) -> Self {
        self.register_runtime = Some(cb);
        self
    }

    /// Set deregister callback: called when a child agent thread finishes (ok/error/cancel).
    /// Parameters: &str (thread_id)
    pub fn with_deregister_runtime(mut self, cb: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        self.deregister_runtime = Some(cb);
        self
    }

    /// 注入 main agent 在 session/new 时捕获的 frozen CLAUDE.md/Skills 数据。
    /// 透传给 SubAgentTool，确保所有 SubAgent 调用复用同一份 frozen 数据，
    /// 避免文件中途变更导致 SubAgent 行为漂移（违反第一优先级不变量）。
    pub fn with_frozen_data(
        mut self,
        claude_md: Option<Arc<String>>,
        claude_local_md: Option<Arc<String>>,
        skill_summary: Option<Arc<String>>,
    ) -> Self {
        self.frozen_claude_md = claude_md;
        self.frozen_claude_local_md = claude_local_md;
        self.frozen_skill_summary = skill_summary;
        self
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
        if let Some(ref registry) = self.background_registry {
            tool = tool.with_background_registry(Arc::clone(registry));
        }
        if !self.registered_hooks.is_empty() {
            tool = tool.with_registered_hooks(self.registered_hooks.to_vec());
        }
        if let Some(ref factory) = self.child_handler_factory {
            tool = tool.with_child_handler_factory(Arc::clone(factory));
        }
        if let Some(ref sender) = self.bg_event_sender {
            tool = tool.with_bg_event_sender(sender.clone());
        }
        if let Some(ref store) = self.thread_store {
            tool = tool.with_thread_store(Arc::clone(store));
        }
        if let Some(ref id) = self.parent_thread_id {
            tool = tool.with_parent_thread_id(id.clone());
        }
        if let Some(ref register) = self.register_runtime {
            tool = tool.with_register_runtime(Arc::clone(register));
        }
        if let Some(ref deregister) = self.deregister_runtime {
            tool = tool.with_deregister_runtime(Arc::clone(deregister));
        }
        // [TRAP] 透传 frozen 数据到 SubAgentTool，避免每轮 build_tool clone 大字符串。
        // Arc::clone 廉价，spawn 时再提取为 String 注入 SubAgentMiddlewareConfig。
        if self.frozen_claude_md.is_some()
            || self.frozen_claude_local_md.is_some()
            || self.frozen_skill_summary.is_some()
        {
            tool = tool.with_frozen_data(
                self.frozen_claude_md.clone(),
                self.frozen_claude_local_md.clone(),
                self.frozen_skill_summary.clone(),
            );
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
/// - 能否并行执行（只读 agent 可安全并发）
/// - 质量/成本/延迟预期（模型级别）
#[derive(Debug, Clone)]
pub struct AgentCapability {
    /// 模型级别：`haiku` / `sonnet` / `opus` / `inherit`
    pub model_tier: String,
    /// 该 agent 是否会修改文件（有 Write/Edit 权限 = true）
    pub can_mutate: bool,
}

/// 从 Agent frontmatter 推断运行时能力画像。
pub fn infer_agent_capability(fm: &ClaudeAgentFrontmatter) -> AgentCapability {
    let model_tier = fm
        .model
        .as_deref()
        .filter(|m| !m.is_empty() && *m != "inherit")
        .unwrap_or("inherit")
        .to_string();

    let disallowed: Vec<String> = fm.disallowed_tools.to_vec();
    let can_mutate = if fm.tools.to_vec().is_empty() {
        // tools 为空 = 继承父工具，除非 Write/Edit 在 disallowed 中
        // 简化：disallowed 含 Write/Edit 则不可变
        !(disallowed.contains(&"Write".to_string()) && disallowed.contains(&"Edit".to_string()))
    } else {
        // 白名单模式：是否含有 Write 或 Edit
        let tools = fm.tools.to_vec();
        tools.contains(&"Write".to_string()) || tools.contains(&"Edit".to_string())
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

#[async_trait]
impl<S: State> Middleware<S> for SubAgentMiddleware {
    fn name(&self) -> &str {
        "SubAgentMiddleware"
    }

    fn collect_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>> {
        let mut tools: Vec<Box<dyn BaseTool>> = vec![Box::new(self.build_tool(cwd))];
        if self.background_registry.is_some() {
            tools.push(Box::new(AgentResultTool::new()));
        }
        tools
    }

    async fn before_agent(&self, state: &mut S) -> AgentResult<()> {
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
