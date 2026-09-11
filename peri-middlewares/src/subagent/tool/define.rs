use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_agent::session::subagent::SubagentHost;
use peri_agent::{
    agent::{events::AgentEventHandler, react::ReactLLM},
    messages::BaseMessage,
    tools::BaseTool,
};
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::invocation::InvocationArgs;
use crate::tool_search::core_tools::TOOL_AGENT;
use crate::{agent_define::AgentOverrides, hooks::types::RegisteredHook, mcp::McpAgentRegistry};

/// SubAgentTool - implements the `Agent` tool, allowing LLM to delegate sub-tasks to specialized sub-agents
const AGENT_DESCRIPTION: &str = include_str!("descriptions/agent.md");

/// SubAgentTool（L3 瘦身）：只声明工具与发起意图，不持有创建实现。
///
/// 创建（建 thread / 建 session / 运行 / 收尾）统一经
/// [`SessionFactory::spawn_subagent`](peri_agent::session::subagent::SessionFactory::spawn_subagent)（peri-agent `SessionFactory` 统一入口）。父侧运行时通道
/// （thread_store / task_manager / bg 事件 / register / deregister / frozen
/// 回退值）聚合在 [`SubagentHost`]；生产路径经 `parent_session` 的 host 读取
/// （builder 在主 session 创建后注入），测试/遗留路径经 `with_*` 直接注入
/// tool 的 host 回退。
pub struct SubAgentTool {
    /// Parent agent tool set (Arc shared, read-only)
    pub(crate) parent_tools: Arc<Vec<Arc<dyn BaseTool>>>,
    /// Parent agent event handler (transparent forwarding of sub-agent events)
    pub(crate) event_handler: Option<Arc<dyn AgentEventHandler>>,
    /// Parent agent working directory (inherited when LLM does not specify cwd)
    pub(crate) parent_cwd: String,
    /// LLM factory function, creates independent LLM instance for each sub-agent (no system, injected via with_system_prompt())
    #[allow(clippy::type_complexity)]
    pub(crate) llm_factory:
        Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
    /// System prompt builder: (agent overrides, cwd) -> system prompt string
    #[allow(clippy::type_complexity)]
    pub(crate) system_builder:
        Option<Arc<dyn Fn(Option<&AgentOverrides>, &str) -> String + Send + Sync>>,
    /// Optional cancellation token for interrupting sub-agent execution
    pub(crate) cancel: Option<AgentCancellationToken>,
    /// Shared reference to parent agent message snapshot (used by Fork path)
    pub(crate) parent_messages: Option<Arc<RwLock<Vec<BaseMessage>>>>,
    /// 子 agent 生命周期 hook（SubagentStart/SubagentStop；构造 lifecycle 闭包用）
    pub(crate) registered_hooks: Arc<Vec<RegisteredHook>>,
    /// Per-child event handler factory
    #[allow(clippy::type_complexity)]
    pub(crate) child_handler_factory:
        Option<Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>>,
    /// 父 agent 的 v2 事件侧 AgentId（共享 cell，由 peri-acp builder 在
    /// 主 v2 session 创建后注入；None = 未注入/测试路径 → 不 emit v2 Start/Stop）。
    pub(crate) parent_agent_id: Arc<RwLock<Option<AgentId>>>,
    /// 父 v2 session（L3）：builder 在主 session 创建后注入；运行时通道经
    /// `parent_session.subagent_host()` 读取。
    pub(crate) parent_session: Arc<RwLock<Option<Arc<peri_agent::session::Session>>>>,
    /// 运行时通道回退值（测试/遗留路径经 with_* 注入；生产路径为默认空，
    /// 由 parent_session 的 host 覆盖）
    pub(crate) host: SubagentHost,
    /// 已启用插件提供的 agent definition 目录。
    pub(crate) plugin_agent_dirs: Arc<Vec<std::path::PathBuf>>,
    /// 会话级 MCP Agent registry。远端定义只在显式选择后读取和批准。
    pub(crate) mcp_agent_registry: Option<Arc<McpAgentRegistry>>,
    /// 用户交互 broker，用于远端 Agent 内容绑定批准。
    pub(crate) broker: Option<Arc<dyn peri_agent::interaction::UserInteractionBroker>>,
    /// 子链装配器（middlewares 实现，链序契约 ARC-MIDDLEWARE-001）
    pub(crate) chain_assembler: Arc<dyn peri_agent::session::subagent::SubagentChainAssembler>,
}

#[async_trait]
impl BaseTool for SubAgentTool {
    fn name(&self) -> &str {
        TOOL_AGENT
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// 提示词层声明分组（design v2 §2.5.1）：交互类工具归入 `interaction`。
    fn namespace(&self) -> Option<&str> {
        Some("interaction")
    }

    /// 提示词层声明模板（design v2 §2.5.3）：委派独立子任务/专业工作。
    ///
    /// title 不覆盖——走 `BaseTool::tool_description` 默认路径由 name 推导。
    /// 05_using_tools.md 手写条目在渐进迁移完成前保留（守护测试防逐字重复）。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Hand off independent or specialized tasks → `{{name}}` ({{title}}). Agent types and usage live in the SubAgent docs."
                .to_string(),
        )
    }

    fn description(&self) -> &str {
        AGENT_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            // resume_thread_id 存在时 prompt 可缺省（隐式继续），故 required 恒空；
            // 非 resume 路径缺 prompt 仍由 invoke 运行时校验兜底（语义不变）
            "required": [],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task description to delegate to the sub-agent. Must be clear and self-contained, as the sub-agent has no access to the parent conversation history. Include all necessary context"
                },
                "resume_thread_id": {
                    "type": "string",
                    "description": "可选，默认不填：不填即新建 subagent。仅当要恢复此前被中断/失败的 subagent 时才填：值为其 child_thread_id（从之前 Agent 调用的返回/错误文本或 bg 通知中获得，恒为 UUID）。提供时从磁盘 thread 恢复现场继续执行，不创建新 subagent，且优先于 subagent_type / fork（两者被忽略）；prompt 可选（缺省隐式继续）；可与 run_in_background 组合（恢复后按此模式执行）。thread 状态须非 active"
                },
                "description": {
                    "type": "string",
                    "description": "A short description of the task (3-5 words), used for UI display and logging"
                },
                "subagent_type": {
                    "type": "string",
                    "description": "The agent ID from the available agents list (e.g., 'code-reviewer', 'explorer'). Must exactly match an agent definition file at .claude/agents/{subagent_type}.md or .claude/agents/{subagent_type}/agent.md. REQUIRED for NEW sub-agents unless fork=true (when not provided and fork is not set, the call will fail). Ignored when resume_thread_id is provided (resume takes priority over subagent_type / fork)"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model tier override, only applies to NEW defined-type sub-agents (subagent_type path): overrides the `model` declared in the agent definition frontmatter; when omitted, the definition's model is used. Available tiers: 'inherit' (use the parent agent's model), 'haiku' (fastest/cheapest, best for quick lookups), 'sonnet' (balanced default), 'opus' (strongest reasoning), 'fable' (flagship tier). Within the defined-type path, unknown values are rejected with an error — never silently ignored. Ignored (not validated) when fork=true (forks always inherit the parent model) and when resume_thread_id is provided (resume keeps the original execution context)"
                },
                "name": {
                    "type": "string",
                    "description": "A short alias for the sub-agent, used for UI identification"
                },
                "isolation": {
                    "type": "string",
                    "description": "Isolation mode for the sub-agent. Use 'worktree' to create an isolated git worktree. Currently reserved for future use"
                },
                "run_in_background": {
                    "type": "boolean",
                    "description": "Set to true to run the sub-agent in the background. The main agent continues immediately and receives a notification when the background task completes. Maximum 3 concurrent background tasks"
                },
                "cwd": {
                    "type": "string",
                    "description": "The working directory for the sub-agent. Defaults to inheriting the parent agent's current working directory if not specified"
                },
                "fork": {
                    "type": "boolean",
                    "description": "Set to true to fork the current agent with full conversation context. The forked agent inherits all messages, tools, and system prompt from the parent. Use when the task requires context from the ongoing conversation. Mutually exclusive with subagent_type: when fork=true, do NOT provide subagent_type (new sub-agents and forks are alternative modes)"
                }
            }
        })
    }

    fn aliases(&self) -> &[&str] {
        &["task"]
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let InvocationArgs {
            resume_thread_id,
            prompt,
            subagent_type,
            model,
            run_in_background,
            cwd,
            is_fork,
        } = InvocationArgs::parse(&input, &self.parent_cwd);

        // host 提前获取（resume 校验需要 thread_store；R-M2 分支优先级）
        let host = self.host();

        // ── resume 分支（优先于 bg / fork / agent-def，R-M2）──
        // 容错语义：resume_thread_id 为有效 UUID 时直接进入恢复分支，subagent_type /
        // fork 字段被忽略（LLM 常按 schema 惯性同时携带，报错会让恢复被拦两次而放弃；
        // 宽容处理使恢复总是可成功，多余字段无副作用）。非 UUID 占位符已在解析时
        // 过滤（见上），不会劫持新建路径。
        // 恢复需要持久化现场：磁盘 thread 是恢复的唯一来源（无 thread_store 无法恢复）
        if resume_thread_id.is_some() && host.thread_store.is_none() {
            return Err("Error: resume_thread_id requires a thread store".into());
        }
        if let Some(thread_id) = resume_thread_id.as_ref() {
            return self
                .invoke_resume(thread_id.clone(), prompt, cwd, run_in_background)
                .await;
        }

        // 非 resume 路径：prompt 必填（required:[] 后由运行时校验兜底）
        let Some(prompt) = prompt else {
            return Err("Error: missing required parameter prompt".into());
        };

        let current_messages = self.current_messages(_ctx.messages);

        let is_mcp_agent = subagent_type
            .as_deref()
            .is_some_and(|id| id.starts_with("mcp__"));
        if is_mcp_agent && run_in_background {
            return Err("Error: MCP Agents currently support synchronous activation only".into());
        }

        // 后台路径需要 task_manager（L3：经 parent_session 的 host 或 tool host 回退）。
        // resume_thread_id.is_none() 为双保险（R-M2）：resume 分支已先返回，此处不可能
        // 再有 resume 调用——防止未来分支重排时 resume 被 bg 分支静默吞掉。
        if resume_thread_id.is_none() && run_in_background && host.task_manager.is_some() {
            return self
                .invoke_background(
                    prompt,
                    subagent_type,
                    cwd,
                    is_fork,
                    current_messages,
                    model.as_deref(),
                )
                .await;
        }

        if is_fork {
            return self.invoke_fork(&prompt, &cwd, current_messages).await;
        }

        let agent_id = match &subagent_type {
            Some(id) => id.clone(),
            None => {
                return Err(
                    "Error: please provide subagent_type parameter to specify the agent type, or use fork: true for fork mode"
                        .into(),
                )
            }
        };

        let agent_def = if is_mcp_agent {
            self.load_and_approve_mcp_agent(&agent_id).await?
        } else {
            match self.load_agent_def(&agent_id, &cwd) {
                Ok(agent) => agent,
                Err(error) => return Err(error.into()),
            }
        };

        let build_result = self
            .build_agent_from_def(
                &agent_def,
                &agent_id,
                &cwd,
                peri_agent::session::subagent::SubagentCancelPolicy::Cascade,
                false,
                true,
                model.as_deref(),
            )
            .await?;

        let llm = build_result.llm;

        let config = self.spawn_config_base(
            agent_id.clone(),
            prompt.clone(),
            Vec::new(),
            peri_agent::session::subagent::SubagentCancelPolicy::Cascade,
            build_result.max_iterations,
            None, // agent 定义路径不包装 fork directive
            peri_agent::session::subagent::SubagentRunMode::Sync,
            llm,
            build_result
                .tools
                .into_iter()
                .map(|t| Arc::from(t) as Arc<dyn BaseTool>)
                .collect(),
            build_result.tool_filter,
            build_result.system_prompt,
            build_result.skill_names,
            cwd,
        );

        let spawned = self.spawn(config).await?;

        // Interrupted 语义与迁移前一致；文本携带 child_thread_id——主 agent 凭此
        // 找回执行现场（thread_store 为 None 的测试路径同样带 id：spawned.child_thread_id 恒可用）
        if spawned.interrupted {
            return Ok(format!(
                "child_thread_id: {}\nSub-agent execution was interrupted, resume with Agent(resume_thread_id: {})",
                spawned.child_thread_id, spawned.child_thread_id
            ));
        }

        if host.thread_store.is_some() {
            Ok(format!(
                "child_thread_id: {}
{}",
                spawned.child_thread_id,
                format_subagent_result(&peri_agent::agent::react::AgentOutput {
                    text: extract_last_ai_text(&spawned.session),
                    steps: 0,
                    tool_calls: Vec::new(),
                    stop_reason: None,
                    block_continue: None,
                })
            ))
        } else {
            Ok(format_subagent_result(
                &peri_agent::agent::react::AgentOutput {
                    text: extract_last_ai_text(&spawned.session),
                    steps: 0,
                    tool_calls: Vec::new(),
                    stop_reason: None,
                    block_continue: None,
                },
            ))
        }
    }
}

/// 复用 peri-agent 的 subagent 结果格式与文本提取
use peri_agent::session::subagent::{extract_last_ai_text, format_subagent_result};
