use super::{
    async_trait, fmt, AgentCancellationToken, AgentEventHandler, AgentId, Arc, BaseMessage,
    BaseTool, Debug, Formatter, InvocationArgs, McpAgentRegistry, RegisteredHook, RwLock,
    SubagentHost, TOOL_AGENT,
};

/// SubAgentTool - implements the `Agent` tool, allowing LLM to delegate sub-tasks to specialized sub-agents
pub(super) const AGENT_DESCRIPTION: &str = include_str!("../descriptions/agent.md");

/// SubAgentTool（L3 瘦身）：只声明工具与发起意图，不持有创建实现。
///
/// 创建（建 thread / 建 session / 运行 / 收尾）统一经
/// [`SessionFactory::spawn_subagent`](peri_agent::session::subagent::SessionFactory::spawn_subagent)（peri-agent `SessionFactory` 统一入口）。父侧运行时通道
/// （session_resources / task_manager / bg 事件 / register / deregister / frozen
/// 回退值）聚合在 [`SubagentHost`]；生产路径经 `parent_session` 的 host 读取
/// （builder 在主 session 创建后注入），测试/遗留路径经 `with_*` 直接注入
/// tool 的 host 回退。
#[derive(Clone)]
pub struct SubAgentTool {
    pub(crate) inherited_tool_filter: peri_agent::session::tool_catalog::ToolFilter,
    /// Parent agent tool set (Arc shared, read-only)
    pub(crate) parent_tools: Arc<Vec<Arc<dyn BaseTool>>>,
    /// Parent agent event handler (transparent forwarding of sub-agent events)
    pub(crate) event_handler: Option<Arc<dyn AgentEventHandler>>,
    /// Parent agent working directory (inherited when LLM does not specify cwd)
    pub(crate) parent_cwd: String,
    /// 子模型工厂（H1）：只产出模型来源；身份 system 与请求时 contribution
    /// provider 由 Agent 层 session factory 在子链装配点统一装上。
    #[allow(clippy::type_complexity)]
    pub(crate) llm_factory:
        Arc<dyn Fn(Option<&str>) -> peri_agent::session::subagent::SubagentLlmSource + Send + Sync>,
    /// System prompt builder: (agent overrides, cwd) -> system prompt string
    #[allow(clippy::type_complexity)]
    pub(crate) system_builder: Option<peri_agent::session::factory::SystemPromptBuilder>,
    /// Optional cancellation token for interrupting sub-agent execution
    pub(crate) cancel: Option<AgentCancellationToken>,
    /// Shared reference to parent agent message snapshot (used by Fork path)
    pub(crate) parent_messages: Option<Arc<RwLock<Vec<BaseMessage>>>>,
    /// 子 agent 生命周期 hook（SubagentStart/SubagentStop；构造 lifecycle 闭包用）
    pub(crate) registered_hooks: Arc<Vec<RegisteredHook>>,
    /// 生命周期 hook 分发器（懒建，由父 middleware 共享给每次目录重绑的工具）。
    ///
    /// [TRAP] 不能按 spawn 新建：`once:true` 的 SubagentStart/Stop 依赖共享
    /// [`OnceTracker`](crate::hooks::once_tracker::OnceTracker)，每次 spawn 新建
    /// dispatcher 会让 once 随每个子 agent 重新触发。独立构造的工具持有自己的 cell。
    /// `None` = registered_hooks 为空。
    pub(crate) lifecycle_dispatcher:
        Arc<std::sync::OnceLock<Option<Arc<crate::hooks::dispatcher::HookDispatcher>>>>,
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
    /// 会话级 MCP Agent registry。远端定义只在显式选择后读取和批准。
    pub(crate) mcp_agent_registry: Option<Arc<McpAgentRegistry>>,
    /// 用户交互 broker，用于远端 Agent 内容绑定批准。
    pub(crate) broker: Option<Arc<dyn peri_agent::interaction::UserInteractionBroker>>,
    /// 子链装配器（middlewares 实现，链序契约 ARC-MIDDLEWARE-001）
    pub(crate) chain_assembler: Arc<dyn peri_agent::session::subagent::SubagentChainAssembler>,
}

impl Debug for SubAgentTool {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubAgentTool")
            .field("parent_cwd", &self.parent_cwd)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl BaseTool for SubAgentTool {
    fn context_retention(&self) -> peri_agent::tools::ContextRetention {
        peri_agent::tools::ContextRetention::Report
    }

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
                    "description": "Task instructions for a new or resumed sub-agent. With resume_thread_id targeting an active background sub-agent, this is a required non-empty supplemental message, queued as Defer to drive a subsequent model call without interrupting or restarting it. For new sub-agents, include all necessary context"
                },
                "resume_thread_id": {
                    "type": "string",
                    "description": "目标 subagent 的 child_thread_id（UUID，即回执/通知中 `child_thread_id: ...` 标注的值）；不填即新建。它不是 task id：后台 task id 形如 `bg-<uuid>`，去掉 `bg-` 前缀后的 UUID 仍不是线程身份（形态合法但查无此线程，会以 thread not found 拒绝）。非 UUID 的非空值（例如后台 task_id `bg-...`）一样被明确拒绝，而不是忽略后新建。active 后台执行：将非空 prompt 作为 Defer 入队并立即返回 action: send / status: queued，不中断、不恢复，在后续 Receive 驱动推理。非 active：从磁盘恢复，prompt 可省略以隐式继续；恢复恒为后台。两种行为均优先于 subagent_type / fork。active 但当前会话没有可投递运行实例时明确报错"
                },
                "send_only": {
                    "type": "boolean",
                    "default": false,
                    "description": "When true, only queue a non-empty prompt to the live background sub-agent identified by resume_thread_id. If it has no live receiver, fail without loading or resuming a persisted thread."
                },
                "description": {
                    "type": "string",
                    "description": "可选的客户端 UI 显示摘要，仅在省略 prompt 时作为 fallback；不参与执行语义或服务端日志"
                },
                "subagent_type": {
                    "type": "string",
                    "description": "Exact agent ID from the available catalog or a current loader suggestion. The loader resolves definitions from project configuration, enabled plugins, built-in providers, or MCP resources; a project-local definition file is not required. REQUIRED for NEW sub-agents unless fork=true (when not provided and fork is not set, the call will fail). Ignored when resume_thread_id is provided (resume takes priority over subagent_type / fork)"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model tier override, only applies to NEW defined-type sub-agents (subagent_type path): overrides the `model` declared in the agent definition frontmatter; when omitted, the definition's model is used. Available tiers: 'inherit' (use the parent agent's model), 'haiku' (fastest/cheapest, best for quick lookups), 'sonnet' (balanced default), 'opus' (strongest reasoning), 'fable' (flagship tier). Within the defined-type path, unknown values are rejected with an error — never silently ignored. Ignored (not validated) when fork=true (forks always inherit the parent model) and when resume_thread_id is provided (resume 从当前磁盘上的 Agent 定义重建模型，不保留先前的 model override，本次 resume 调用的 model 参数也被忽略)"
                },
                "cwd": {
                    "type": "string",
                    "description": "默认继承父 Agent 的 cwd。显式路径必须存在，且 canonicalization 后与父 cwd 相同（允许 symlink、dotdot、尾部斜杠等价路径）；不同目录会被拒绝，包括 resume 调用。不支持独立子 Workspace 重绑定"
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
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.invoke_output(input, ctx).await?.text)
    }

    async fn invoke_output(
        &self,
        input: serde_json::Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<peri_agent::tools::ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
        use peri_agent::tools::ToolOutput;
        let InvocationArgs {
            resume_thread_id,
            send_only,
            prompt,
            subagent_type,
            model,
            cwd,
            is_fork,
            ..
        } = InvocationArgs::parse(&input)?;
        let cwd = self.validate_cwd(cwd.as_deref()).await?;
        if send_only {
            let thread_id = resume_thread_id.expect("send_only parsing requires resume_thread_id");
            let prompt = prompt.expect("send_only parsing requires a non-empty prompt");
            return self
                .send_only(&thread_id, &prompt)
                .await
                .map(ToolOutput::from_legacy);
        }

        // 父侧模型 tool-call ID 传给子 agent 配置（配置层仍是字符串形状）。
        let parent_tool_call_id = ctx
            .tool_call_id
            .as_ref()
            .map(|tool_call_id| tool_call_id.as_str().to_string());

        // 本调用的交互归属（MCP Agent 内容绑定审批用）：来自 canonical dispatch
        // 绑定的真实 turn 事实；缺失时审批路径显式拒绝，不伪造归属。
        let approval_owner = crate::interaction_owner::owner_from_tool_context(&ctx);

        // ── resume 分支（优先于 fork / agent-def，R-M2）──
        // 容错语义：resume_thread_id 为有效 UUID 时直接进入恢复分支，subagent_type /
        // fork 字段被忽略（LLM 常按 schema 惯性同时携带，报错会让恢复被拦两次而放弃；
        // 宽容处理使恢复总是可成功，多余字段无副作用）。
        // 非 UUID 非空值已在解析时报明确错误（invocation.rs），不会劫持新建路径，
        // 也不会被静默丢弃后转入新建。
        // invoke_resume 先尝试当前会话的 live Defer 投递；只有恢复路径才需要磁盘。
        //
        // 非 active 恢复恒后台唤醒；active 线程的 Defer 投递不需要执行模式。
        if let Some(thread_id) = resume_thread_id.as_ref() {
            return self
                .invoke_resume(
                    thread_id.clone(),
                    prompt,
                    cwd,
                    parent_tool_call_id.clone(),
                    approval_owner,
                )
                .await
                .map(ToolOutput::from_legacy);
        }

        // 非 resume 路径：prompt 必填（required:[] 后由运行时校验兜底）
        let Some(prompt) = prompt else {
            return Err("Error: missing required parameter prompt".into());
        };

        // ── Fork 上下文门（启动前，所有 fork 共用同一决策点）──
        // 父会话上下文使用率高于上限时 fork 不可用：拒绝并引导改用非 fork
        // 子 agent（subagent_type）或 resume。resume 分支已先返回，不经过本门。
        if is_fork {
            crate::subagent::fork::fork_context_usage_gate(ctx.context_usage)?;
        }

        // Agent 调用恒后台（Issue #450）：没有同步分支，也没有模式参数。
        // 旧客户端发送的 `run_in_background` 仅在解析层记录并忽略。
        if is_fork {
            // 只有 fork 消费父历史；命名子 agent 从零构建 transcript。
            let parent_messages = self.current_messages(ctx.messages);
            return self
                .invoke_background_fork(prompt, cwd, parent_messages, parent_tool_call_id)
                .await
                .map(ToolOutput::from_legacy);
        }

        let agent_id = match subagent_type {
            Some(id) => id,
            None => {
                let error = "Error: please provide subagent_type parameter to specify the agent type, or use fork: true for fork mode";
                return Err(self.agent_error_with_suggestions(error, None).into());
            }
        };
        let is_mcp_agent = agent_id.starts_with("mcp__");
        // 定义准入在后台启动之前完成：MCP Agent 必须经内容绑定审批门，
        // 后台 loader（`load_agent_def`）不得成为绕过审批的入口。
        let agent_def = if is_mcp_agent {
            self.load_and_approve_mcp_agent(&agent_id, approval_owner.as_ref())
                .await?
        } else {
            match self.load_agent_def(&agent_id).await {
                Ok(agent) => agent,
                Err(error) => {
                    return Err(self
                        .agent_error_with_suggestions(&error, Some(&agent_id))
                        .into());
                }
            }
        };

        self.invoke_background_agent(
            agent_id,
            agent_def,
            prompt,
            cwd,
            model.as_deref(),
            parent_tool_call_id,
        )
        .await
        .map(ToolOutput::from_legacy)
    }
}
