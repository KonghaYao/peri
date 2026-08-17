use std::sync::Arc;

use peri_acp_types::command::{BgForkRequest, BgForkSpawner};
use peri_acp_types::tasks::TaskManager;

use crate::agent::{async_tasks::TaskManager as AgentTaskManager, react::ReactLLM};
use crate::session::subagent::{SubagentChainAssembler, SubagentHost};
use crate::tools::{BaseTool, ToolInvocationResolver};

/// 父工具集构造闭包（/bg fork 惰性构建；ACP 侧 middlewares 实现注入）。
pub type ParentToolsFactory = Arc<dyn Fn() -> Arc<Vec<Arc<dyn BaseTool>>> + Send + Sync>;

/// LLM 构造闭包（/bg fork 惰性构造；ACP 侧 `peri_config` 自持）。
pub type ForkLlmFactory =
    Arc<dyn Fn() -> Result<Box<dyn ReactLLM + Send + Sync>, String> + Send + Sync>;

/// `/bg` fork agent 启动器默认实现（L5：自 ACP 过渡宿主迁入，装配注入面化）。
///
/// 深绑 Agent 层 `SessionFactory`（L3 迁出后经统一入口调用）：LLM 构造 /
/// 父工具集 / SubAgent 发起在本实现内完成；命令定义（`session/exec/bg.rs`）
/// 只经 [`BgForkSpawner`] 接口发起，不引用业务面实现。
/// LLM 构造 / 父工具集 / 链装配器 / tool resolver 由装配面经 [`Self::new`]
/// 注入（ACP 侧自持会话级配置与 middlewares 实现）。
pub struct DefaultBgForkSpawner {
    task_manager: Arc<dyn TaskManager>,
    /// 主 LLM 构造工厂（ACP 装配面自持 `peri_config`；惰性调用）。
    llm_factory: ForkLlmFactory,
    /// 父工具集构造工厂（文件系统 + 终端 + Web；惰性调用，仅 /bg 触发时构建）。
    parent_tools_factory: ParentToolsFactory,
    /// 子 agent 链装配器（middlewares 实现，链序契约 ARC-MIDDLEWARE-001）。
    chain_assembler: Arc<dyn SubagentChainAssembler>,
    /// deferred 工具解析器（wrapper-aware canonical resolver）。
    tool_invocation_resolver: Arc<dyn ToolInvocationResolver>,
}

impl DefaultBgForkSpawner {
    /// 装配注入构造（ACP 装配面调用；LLM 构造配置由调用方自持捕获）。
    pub fn new(
        task_manager: Arc<dyn TaskManager>,
        llm_factory: ForkLlmFactory,
        parent_tools_factory: ParentToolsFactory,
        chain_assembler: Arc<dyn SubagentChainAssembler>,
        tool_invocation_resolver: Arc<dyn ToolInvocationResolver>,
    ) -> Self {
        Self {
            task_manager,
            llm_factory,
            parent_tools_factory,
            chain_assembler,
            tool_invocation_resolver,
        }
    }
}

#[async_trait::async_trait]
impl BgForkSpawner for DefaultBgForkSpawner {
    async fn spawn_fork(&self, req: BgForkRequest) -> Result<(), String> {
        // 并发限制（迁移前由 spawn_background_fork 内部预检，错误文案保持）
        if self.task_manager.active_count() >= 3 {
            return Err("已有 3 个后台任务在运行".to_string());
        }

        // 构造 LLM 实例（经注入工厂；L5 依赖反转）
        let llm: Box<dyn ReactLLM + Send + Sync> = (self.llm_factory)()?;

        // 构造父工具集（文件系统 + 终端 + Web = Read/Write/Edit/Bash/Grep/Glob/WebFetch/WebSearch）
        // NOTE: MCP tools are intentionally excluded because:
        // 1. Background workers should not depend on external MCP servers that may be unavailable
        // 2. MCP tools may require interactive approval, which doesn't work for background agents
        // 3. Core filesystem + terminal + web tools cover the majority of background task use cases
        let parent_tools: Arc<Vec<Arc<dyn BaseTool>>> = (self.parent_tools_factory)();

        // 装配注入的 per-session TaskManager 实现（L1：BackgroundTaskRegistry
        // per-session 实例化）；SubAgent 发起面需要具体类型，经 trait 对象
        // downcast 还原——非 Agent 层实现（如 NoopTaskManager）时优雅报错。
        let concrete_tm: Option<Arc<AgentTaskManager>> = {
            let tm_any: Arc<dyn std::any::Any + Send + Sync> =
                Arc::clone(&self.task_manager) as Arc<dyn std::any::Any + Send + Sync>;
            tm_any.downcast::<AgentTaskManager>().ok()
        };
        let Some(concrete_tm) = concrete_tm else {
            return Err(
                "task_manager 实现不支持 /bg（需 Agent 层 per-session TaskManager）".to_string(),
            );
        };

        // L3：/bg 经 Agent 层统一入口 spawn_subagent（parent 缺失：无主 session 对象，
        // 父侧数据经 config 显式携带；frozen 数据来自注入的冻结值，不重读磁盘）。
        let host = SubagentHost {
            thread_store: Some(req.thread_store.clone()),
            task_manager: Some(Arc::clone(&concrete_tm)),
            bg_event_sender: Some(req.bg_event_sender.clone()),
            on_bg_complete: None, // /bg 命令的主 agent 不在 loop，注入无效
            register_runtime: None,
            deregister_runtime: None,
            langfuse_bridge: None, // /bg 命令无 Langfuse tracer
            frozen_claude_local_md: req
                .frozen_claude_local_md
                .as_ref()
                .map(|s| Arc::new(s.clone())),
            frozen_system_prompt: req
                .frozen_system_prompt
                .as_ref()
                .map(|s| Arc::new(s.clone())),
            parent_thread_id: req.parent_thread_id.clone(),
            frozen_claude_md: req.frozen_claude_md.as_ref().map(|s| Arc::new(s.clone())),
            frozen_skill_summary: req
                .frozen_skill_summary
                .as_ref()
                .map(|s| Arc::new(s.clone())),
        };
        let _spawned = match crate::session::subagent::SessionFactory::spawn_subagent(
            None,
            crate::session::subagent::SubagentSpawnConfig {
                agent_name: "fork".to_string(),
                prompt: req.prompt.clone(),
                parent_messages: req.parent_messages.clone(),
                cancel_policy: crate::session::subagent::SubagentCancelPolicy::Independent,
                max_iterations: 200,
                fork_directive_kind: Some(crate::session::subagent::ForkDirectiveKind::Bg),
                run_mode: crate::session::subagent::SubagentRunMode::Background,
                skill_names: Vec::new(),
                llm,
                chain_assembler: Arc::clone(&self.chain_assembler),
                tools: parent_tools
                    .iter()
                    .cloned()
                    .collect::<Vec<Arc<dyn BaseTool>>>(),
                system_prompt: None,
                error_suggest_registry: None,
                tool_registry_snapshot: None,
                tool_invocation_resolver: Some(Arc::clone(&self.tool_invocation_resolver)),
                compact_config: None,
                context_budget: None,
                compact_llm: None,
                thread_store: Some(req.thread_store.clone()),
                event_handler: None,
                bg_event_sender: Some(host.bg_event_sender.clone().unwrap()),
                task_manager: Some(Arc::clone(&concrete_tm)),
                on_bg_complete: None,
                langfuse_bridge: None,
                on_subagent_start: None,
                on_subagent_stop: None,
                register_runtime: None,
                deregister_runtime: None,
                parent_agent_id: None, // /bg 命令无父 agent 身份（不 emit v2 Start/Stop）
                cancel_token: None,    // /bg 独立任务，Independent 策略内部新建
                cwd: Some(req.cwd.clone()),
                parent_thread_id: req.parent_thread_id.clone(),
                frozen_claude_md: req.frozen_claude_md.clone(),
                frozen_claude_local_md: req.frozen_claude_local_md.clone(),
                frozen_skill_summary: req.frozen_skill_summary.clone(),
                frozen_date: None,
            },
        )
        .await
        {
            Ok(s) => s,
            Err(e) => return Err(e.to_string()),
        };

        // P2：v1 SubagentStarted 已移入 spawner 任务内（gate 放行后）经
        // bg_event_sender 发送（bg pump → event_sink），此处不再同步推送——
        // 消除"任务快速完成/被 cancel 时 Stop 先于 Start 到达"的窗口。
        Ok(())
    }
}
