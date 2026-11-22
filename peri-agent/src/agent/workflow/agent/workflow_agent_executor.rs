//! Implementation leaf for `crate::agent::workflow::agent`.

use super::{execution, result};
use peri_acp_types::skills::SkillToolAccess;
pub(super) use std::fmt::{self, Debug, Formatter};
pub(super) use std::sync::Arc;

pub(super) use peri_acp_types::compact::CompactConfig;
pub(super) use peri_acp_types::event::ExecutorEvent;
pub(super) use peri_acp_types::interaction::UserInteractionBroker;
pub(super) use peri_acp_types::messages::BaseMessage;
pub(super) use peri_acp_types::session::MessageKind;
pub(super) use peri_acp_types::session::MessageSource;
pub(super) use peri_acp_types::session::QueuedMessage;
pub(super) use peri_acp_types::thread::AgentStatus;
pub(super) use peri_acp_types::workflow::AgentExecutor;
pub(super) use peri_acp_types::workflow::AgentRunParams;
pub(super) use peri_acp_types::workflow::AgentRunResult;
pub(super) use peri_acp_types::workflow::ProgressEvent;
pub(super) use tokio_util::sync::CancellationToken;
pub(super) use tracing::{debug, warn};

pub(super) use crate::agent::model_bridge::AgentModelBridge;

pub(super) use crate::agent::token::ContextBudget;
pub(super) use crate::agent::workflow::factory::{
    WorkflowAgentFactory, WorkflowAgentPromptBuilder, WorkflowModelFactory, WorkflowPublishHook,
    WorkflowSystemPromptFallback,
};
pub(super) use crate::middleware::chain::MiddlewareChain;
pub(super) use crate::session::exec::executor::LangfuseHooks;
pub(super) use crate::session::exec::executor_helpers::ForwarderLauncherFn;
pub(super) use crate::session::subagent::DefaultSubagentV2ContextBuilder;
pub(super) use crate::session::subagent::SubagentV2ContextBuilder;

pub(super) use super::observation::WorkflowObservation;

/// Langfuse 事件旁路处理器（每条映射后的 v1 ExecutorEvent 调用；构造收
/// ACP 宿主——`UnifiedLangfuseEvent` 映射在 Controller 侧，边 8 禁止
/// 本层直接引用）。
pub type WorkflowLangfuseEventHandler = Arc<dyn Fn(&ExecutorEvent) + Send + Sync>;

/// Workflow agent 构建上下文——携带 session 级 frozen data。
///
/// frozen 数据在 session/new 时捕获，确保 workflow agent 看到的
/// CLAUDE.md / skills 与主会话一致（系统提示词稳定性第一优先级）。
///
/// p1-wa：ACP/Controller/Middleware 特有字段（provider / peri_config /
/// agent_pool / langfuse_session / controller）已端口化为注入闭包与端口
/// （见模块头注释），本结构只承载契约层类型 + 注入面。
///
/// 注：不派生 `Clone`（`LangfuseHooks` 非 Clone；调用点均为构造后 move）。
pub struct WorkflowAgentContext {
    pub cwd: String,
    /// Frozen CLAUDE.md content（含解析的 @import），None = 无文件。
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content，None = 无文件。
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary，None = 无 skills。
    pub frozen_skill_summary: Option<String>,
    /// 会话级 MCP skill registry（W4b/F4：workflow agent 的技能目录与正文来源，
    /// 与主链同一份；None = 未装配技能面 → 两个技能工具为空、预载报缺口，
    /// 不回落磁盘，J5）。
    pub mcp_skill_registry: Option<Arc<peri_acp_types::mcp_skills::McpSkillRegistry>>,

    /// 宿主绑定的调用方会话 ID（compact 事件和日志关联）。
    /// 仅由会话装配注入，不得取自 workflow 参数或模型工具输入。
    pub session_id: Option<String>,
    pub session_resources: Option<Arc<dyn peri_acp_types::session_resources::SessionResources>>,
    /// Compact 配置（None = 不启用自动 compact）
    pub compact_config: Option<CompactConfig>,
    /// 取消令牌（None = workflow agent 创建内部 token）
    pub cancel: Option<CancellationToken>,

    /// 标准 system prompt（session/new 时冻结的 build_system_prompt() 输出）。
    /// None = 回退到注入的 [`WorkflowSystemPromptFallback`] 运行时构建。
    pub system_prompt: Option<String>,
    /// Frozen ACP extension, composed only at the model request boundary.
    pub external_instructions: Option<Arc<str>>,
    pub environment_prompt: Option<Arc<str>>,
    pub legacy_embedded_instructions: bool,
    /// HITL broker + 共享权限模式。两者均 Some 时启用审批；
    /// 任一为 None 时 Bypass（自主后台 agent 默认行为）。
    pub broker: Option<Arc<dyn UserInteractionBroker>>,
    pub permission_mode: Option<Arc<peri_acp_types::permission::SharedPermissionMode>>,

    /// Frozen date + language（system prompt fallback 构建时的日期/语言一致性）。
    pub frozen_date: Option<String>,
    pub frozen_language: Option<String>,

    /// 进度事件发送通道（None = 不发送 agent_progress 事件）
    pub progress_tx: Option<tokio::sync::mpsc::UnboundedSender<ProgressEvent>>,

    /// subagent v2 上下文构建器（None = 回退到
    /// `DefaultSubagentV2ContextBuilder`，与迁移前一致）。
    pub subagent_ctx_builder: Option<Arc<dyn SubagentV2ContextBuilder>>,
    /// 指定 `agentType` 时渲染相同的 subagent prompt 覆盖。
    pub agent_prompt_builder: WorkflowAgentPromptBuilder,

    // ── p1-wa 注入面（依赖反转，见模块头注释）────────────────────────────
    /// 模型工厂（ACP 宿主构造：alias 解析 + retry observer 烘焙）。
    pub model_factory: WorkflowModelFactory,
    /// 中间件/工具装配端口（peri-middlewares 实现，ACP 宿主装配注入）。
    pub middleware_factory: Arc<dyn WorkflowAgentFactory>,
    /// system prompt fallback 渲染（`system_prompt = None` 时调用）。
    pub system_prompt_fallback: WorkflowSystemPromptFallback,
    /// EventBus forwarder 启动器（ACP 宿主构造）。
    pub forwarder_launcher: ForwarderLauncherFn,
    /// 事件发射钩子（`Controller::publish_event` 适配；None = 无控制面宿主，
    /// 如 print 场景——保持内部消费）。
    pub publish_hook: Option<WorkflowPublishHook>,
    /// Langfuse 观测钩子（turn 开始/结束；None = 遥测禁用——迁移前
    /// `langfuse_session` 恒 None，调用点未接线，保持现状）。
    pub langfuse_hooks: Option<LangfuseHooks>,
    /// Langfuse 事件旁路处理器（每条映射后的 v1 ExecutorEvent 调用；构造收
    /// ACP 宿主——`UnifiedLangfuseEvent` 映射在 Controller 侧，边 8 禁止
    /// 本层直接引用）。
    pub langfuse_event_handler: Option<WorkflowLangfuseEventHandler>,
    /// 装配期关闭的 middleware 名集合（源自父会话冻结状态
    /// `FrozenSessionData::meta_harness().disabled_middlewares`，
    /// 使 workflow agent 链与父会话装配一致——设计 §2.5）。
    pub meta_harness_disabled: std::collections::HashSet<String>,
}

/// Workflow agent executor — builds and runs v2 stages for workflow agent() calls.
pub struct WorkflowAgentExecutor {
    pub(super) ctx: WorkflowAgentContext,
    pub(super) execution_manager: std::sync::OnceLock<Arc<dyn peri_acp_types::tasks::TaskManager>>,
    /// 按 `(run_id, agent_id)` 记录本次执行实际解析的迭代上限与用量。
    ///
    /// 执行结果只携带 agent 输出；上限/用量是宿主结算所需的独立事实，由执行器
    /// （唯一解析点）给出，宿主不得自行推断默认值。`forget_run` 随 run 结束回收。
    execution_limits: parking_lot::RwLock<
        std::collections::HashMap<(String, u64), peri_acp_types::workflow::AgentExecutionLimits>,
    >,
}

impl WorkflowAgentExecutor {
    #[must_use]
    pub fn new(ctx: WorkflowAgentContext) -> Self {
        Self {
            ctx,
            execution_manager: std::sync::OnceLock::new(),
            execution_limits: parking_lot::RwLock::new(std::collections::HashMap::new()),
        }
    }

    fn record_execution_limits(
        &self,
        run_id: &str,
        agent_id: u64,
        limits: peri_acp_types::workflow::AgentExecutionLimits,
    ) {
        let mut recorded = self.execution_limits.write();
        recorded.insert((run_id.to_string(), agent_id), limits);
    }
}

/// 创建携带 frozen data 的 workflow agent executor。
#[must_use]
pub fn create_executor(ctx: WorkflowAgentContext) -> Arc<dyn AgentExecutor> {
    Arc::new(WorkflowAgentExecutor::new(ctx))
}

/// 便捷工厂：创建无 frozen data 的 workflow agent executor。
///
/// p1-wa 备注：调用方均已迁至注入面构造（`host/prompt.rs` /
/// `host/stdio/session/prompt_exec.rs` 直构 `WorkflowAgentContext`），
/// 本函数为 API 兼容保留（dead code 候选，删除留待 API 冻结窗口）。
#[must_use]
pub fn create_default_executor(
    model_factory: WorkflowModelFactory,
    middleware_factory: Arc<dyn WorkflowAgentFactory>,
    system_prompt_fallback: WorkflowSystemPromptFallback,
    forwarder_launcher: ForwarderLauncherFn,
    cwd: &str,
) -> Arc<dyn AgentExecutor> {
    Arc::new(WorkflowAgentExecutor::new(WorkflowAgentContext {
        cwd: cwd.to_string(),
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        mcp_skill_registry: None,
        session_id: None,
        session_resources: None,
        compact_config: None,
        cancel: None,
        system_prompt: None,
        external_instructions: None,
        environment_prompt: None,
        legacy_embedded_instructions: false,
        broker: None,
        permission_mode: None,
        frozen_date: None,
        frozen_language: None,
        progress_tx: None,
        subagent_ctx_builder: None,
        agent_prompt_builder: Arc::new(|_, _, _, _, _| String::new()),
        model_factory,
        middleware_factory,
        system_prompt_fallback,
        forwarder_launcher,
        publish_hook: None,
        langfuse_hooks: None,
        langfuse_event_handler: None,
        meta_harness_disabled: std::collections::HashSet::new(),
    }))
}

/// 有效迭代上限与来源的唯一解析点。
///
/// 定义给出正向 `max_turns`（含 builtin coder 的 400）时按定义值；未指定 agentType
/// 或定义为 0 时回落执行器默认值。调用方（attempt/state 投影）直接使用这里的结果，
/// 不另行推断或写死默认。
pub(super) fn resolve_iteration_limit(
    agent_definition: Option<&crate::agent::workflow::factory::WorkflowAgentDefinition>,
) -> (usize, peri_acp_types::workflow::IterationLimitSource) {
    match agent_definition
        .map(|definition| definition.max_iterations)
        .filter(|max_iterations| *max_iterations > 0)
    {
        Some(limit) => (
            limit,
            peri_acp_types::workflow::IterationLimitSource::AgentDefinition,
        ),
        None => (200, peri_acp_types::workflow::IterationLimitSource::Default),
    }
}

pub(super) fn requested_model<'a>(
    request_model: Option<&'a str>,
    agent_definition: Option<&'a crate::agent::workflow::factory::WorkflowAgentDefinition>,
) -> Option<&'a str> {
    fn normalize(model: &str) -> Option<&str> {
        let model = model.trim();
        (!model.is_empty() && !model.eq_ignore_ascii_case("inherit")).then_some(model)
    }

    match request_model {
        Some(model) => normalize(model),
        None => agent_definition
            .and_then(|definition| definition.model.as_deref())
            .and_then(normalize),
    }
}

#[async_trait::async_trait]
impl AgentExecutor for WorkflowAgentExecutor {
    fn bind_execution_manager(
        &self,
        manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    ) -> Result<(), String> {
        match self.execution_manager.set(manager) {
            Ok(()) => Ok(()),
            Err(manager)
                if self
                    .execution_manager
                    .get()
                    .is_some_and(|current| Arc::ptr_eq(current, &manager)) =>
            {
                Ok(())
            }
            Err(_) => Err("workflow executor is already bound to another session owner".into()),
        }
    }
    async fn execute(&self, params: AgentRunParams) -> AgentRunResult {
        debug!(
            agent_id = params.agent_id,
            label = ?params.label,
            phase = ?params.phase,
            prompt_len = params.prompt.len(),
            allowed_tools = ?params.allowed_tools,
            agent_type = ?params.agent_type,
            requested_model = ?params.model,
            max_tokens = ?params.max_tokens,
            "Workflow agent: starting execution"
        );

        // 重试会复用 `(run_id, agent_id)`：上一轮解析的上限与用量不得挂到本次
        // attempt 上（例如重试在进入循环前就失败）。
        self.execution_limits
            .write()
            .remove(&(params.run_id.clone(), params.agent_id));

        let started_at = peri_time::monotonic_now();

        let agent_definition = match params.agent_type.as_deref() {
            Some(agent_type) => match self
                .ctx
                .middleware_factory
                .resolve_agent_definition(agent_type, &self.ctx.cwd)
                .await
            {
                Ok(definition) => Some(definition),
                Err(detail) => {
                    warn!(agent_type, %detail, "workflow agent: invalid agent type");
                    return AgentRunResult::Dead {
                        scope_close_incomplete: false,
                        reason: Some("invalid-agent-type".into()),
                        detail: Some(detail),
                        duration_ms: Some(started_at.elapsed().as_millis() as u64),
                        token_count: None,
                    };
                }
            },
            None => None,
        };
        if params.max_tokens == Some(0) {
            return AgentRunResult::Dead {
                scope_close_incomplete: false,
                reason: Some("invalid-max-tokens".into()),
                detail: Some("maxTokens must be greater than zero".into()),
                duration_ms: Some(started_at.elapsed().as_millis() as u64),
                token_count: None,
            };
        }

        // 请求的 model 显式覆盖 agent definition；空值 / inherit 表示使用父 provider。
        let requested_model = requested_model(params.model.as_deref(), agent_definition.as_ref());

        // 0. GAP-08: Langfuse turn 开始钩子（注入面；迁移前 `langfuse_session`
        // 恒 None 未接线，None = 遥测禁用）。
        if let Some(ref hooks) = self.ctx.langfuse_hooks {
            (hooks.on_turn_start)(&params.prompt);
        }

        let observation = WorkflowObservation::new(
            &params,
            self.ctx.progress_tx.clone(),
            self.ctx.langfuse_event_handler.clone(),
        );
        let event_handler = observation.handler();

        // ── compact 配置 ──
        // 与主 agent builder 模式一致。必须在 model_factory 调用前构建 context_budget。
        let compact_config = self.ctx.compact_config.clone();
        let context_budget = compact_config.as_ref().map(|cc| {
            ContextBudget::new(ContextBudget::DEFAULT_CONTEXT_WINDOW)
                .with_auto_compact_threshold(cc.auto_compact_threshold)
                .with_warning_threshold(cc.micro_compact_threshold)
        });
        // 本 run 的 retry observer：重试观测直接翻译为 LlmRetrying 交给本地 handler。
        let retry_observer =
            crate::session::retry_events::retry_observer_for(Arc::clone(&event_handler));

        // 模型构造（注入工厂）：compact 与 base 各一份实例（同一 provider 双实例，
        // 与迁移前 `compact_llm` / `base_model` 构造一致）。
        let compact_llm: Option<Arc<dyn peri_model::Model>> = if compact_config.is_some() {
            Some(
                (self.ctx.model_factory)(
                    requested_model,
                    params.max_tokens,
                    retry_observer.clone(),
                )
                .model,
            )
        } else {
            None
        };
        let built_model =
            (self.ctx.model_factory)(requested_model, params.max_tokens, retry_observer);
        let base_model = built_model.model;
        // 有效模型名（alias 解析后；GitAttribution 装配用）。
        let model_name = built_model.model_name;
        // 请求的模型档位（alias 解析成功才有值）；TUI 面板显示档位而非模型名。
        let model_tier = built_model.tier;

        observation.report_model(&model_name, model_tier.clone());

        // 2. 注册工具（端口装配：fs/terminal/web/skills tools，仅 project-level
        // skills——workflow agent 无 plugin_skill_roots）。
        // MetaHarness：disabled 集合源自父会话冻结状态（WorkflowAgentContext
        // 字段，装配实现据此连坐过滤——设计 §2.5）。
        let task_manager = Arc::new(crate::agent::async_tasks::TaskManager::new());
        let mut tools = self.ctx.middleware_factory.build_tools(
            &self.ctx.cwd,
            &self.ctx.meta_harness_disabled,
            Some(task_manager.clone()),
            self.ctx.mcp_skill_registry.clone(),
        );

        // 3. agent definition 工具边界优先，再叠加 workflow allowedTools。
        if let Some(definition) = agent_definition.as_ref() {
            let filter = crate::session::tool_catalog::ToolFilterPolicy::canonical(
                definition.allowed_tools.clone(),
                Vec::new(),
            );
            tools.retain(|tool| {
                filter(tool.as_ref())
                    && definition
                        .allowed_tools
                        .as_ref()
                        .is_none_or(|allowed| tool_name_in(allowed, tool.name()))
                    && !tool_name_in(&definition.disallowed_tools, tool.name())
            });
        }
        if let Some(allowed) = params
            .allowed_tools
            .as_ref()
            .filter(|allowed| !allowed.is_empty())
        {
            let filter = crate::session::tool_catalog::ToolFilterPolicy::canonical(
                Some(allowed.clone()),
                Vec::new(),
            );
            tools.retain(|tool| tool_name_in(allowed, tool.name()) && filter(tool.as_ref()));
        }

        // 4. 指定 agent type 时按相同的 subagent overrides 渲染 prompt；否则
        // 继续复用 session 冻结的默认 subagent prompt。
        let skill_access = SkillToolAccess::from_tools(tools.iter().map(Box::as_ref));
        let system_prompt = select_workflow_system_prompt(
            agent_definition.as_ref(),
            self.ctx.system_prompt.as_deref(),
            &self.ctx.agent_prompt_builder,
            &self.ctx.system_prompt_fallback,
            &self.ctx.cwd,
            self.ctx.frozen_date.as_deref(),
            self.ctx.frozen_language.as_deref(),
            skill_access,
        );

        // 5. 构建中间件链（端口装配；frozen data / HITL 语义自 ctx 读取）
        let mut chain = MiddlewareChain::new();
        let mw_items = self.ctx.middleware_factory.build_middlewares(
            &self.ctx,
            &model_name,
            agent_definition
                .as_ref()
                .map(|definition| definition.skill_names.as_slice())
                .unwrap_or_default(),
            Some(task_manager.clone()),
        );
        for mw in mw_items {
            chain.add(mw);
        }

        // 6. v2 stages 装配（替代 SubAgentBuilder）
        let cancel_token = self.ctx.cancel.clone().unwrap_or_default();
        let (max_iterations, iteration_limit_source) =
            resolve_iteration_limit(agent_definition.as_ref());
        // 上限已解析就先记录上限本身；用量要等循环推进，未进入循环时保持 None。
        self.record_execution_limits(
            &params.run_id,
            params.agent_id,
            peri_acp_types::workflow::AgentExecutionLimits {
                iteration_limit: max_iterations as u64,
                iteration_limit_source,
                iterations_used: None,
            },
        );

        // tools: Vec<Box<dyn BaseTool>> → Vec<Arc<dyn BaseTool>>
        let tools_arc: Vec<Arc<dyn crate::tools::BaseTool>> = tools
            .into_iter()
            .map(|t| Arc::from(t) as Arc<dyn crate::tools::BaseTool>)
            .collect();

        // M4：贡献改由请求时 provider 读取（`before_agent` 已填充链内缓存后
        // 才在 ModelRequest 构造时收集）——与主链/子链同一语义：统一
        // `combine_system_prompt_with_dynamic` 组合、分隔符与 reserved marker
        // 校验由收集器/组合边界负责，不再手工拼接；Skills 在 `before_agent`
        // 生成的贡献因此不再被提前收集漏掉。
        let execution = match execution::WorkflowExecution::prepare(&self.ctx, &params).await {
            Ok(execution) => execution,
            Err(detail) => {
                return AgentRunResult::Dead {
                    scope_close_incomplete: false,
                    reason: Some("execution-blocked".into()),
                    detail: Some(detail),
                    duration_ms: Some(started_at.elapsed().as_millis() as u64),
                    token_count: observation.snapshot().reported_output_tokens(),
                };
            }
        };
        // bridge 与 StageContext 共享同一条链：请求时 provider 读取当前贡献。
        let chain = Arc::new(chain);
        let llm = workflow_model_bridge(
            base_model,
            system_prompt.clone(),
            self.ctx.external_instructions.clone(),
            self.ctx.environment_prompt.clone(),
            self.ctx.legacy_embedded_instructions,
            Arc::clone(&chain),
            &execution.session_id,
        );

        // 构造 v2 StageContext（workflow agent 无 parent_messages）
        // agent_id=None：workflow 无 child_thread_id，内部 AgentId::new() 兜底（C1）
        let ctx_builder = self
            .ctx
            .subagent_ctx_builder
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultSubagentV2ContextBuilder));
        let session_id = execution.session_id.clone();
        let session = crate::session::Session::new_with_cancel_and_queue(
            Arc::from(self.ctx.cwd.as_str()),
            crate::session::FrozenContext::builder().build(),
            Some(session_id.clone()),
            Arc::new(cancel_token.clone()),
            // workflow agent 的待发事实同样落本 thread 的 `thread_mq`。
            crate::session::MessageQueue::bind(execution.resources.clone(), session_id.clone()),
        );
        {
            let transcript = session.transcript();
            let mut transcript = transcript.write();
            *transcript = std::mem::take(&mut *transcript)
                .with_persistence(execution.resources.clone(), session_id.clone());
        }
        let mcp_pool = self.ctx.middleware_factory.mcp_pool();
        session.set_subagent_host(crate::session::subagent::SubagentHost {
            task_manager: Some(task_manager.clone()),
            mcp_pool: mcp_pool.clone(),
            session_resources: Some(execution.resources.clone()),
            ..Default::default()
        });
        // 在线 admission + 订阅收件：本会话的工具任务由自己的 TaskManager 拥有，
        // 必须在首次 MCP 工具调用前按自身地址登记 Inbox/TaskManager（与子会话
        // `subagent/factory/context.rs` 同一步）。未登记时
        // `begin_external_task_execution` 会以 "session task manager unavailable"
        // 拒绝每个工具调用，scope 也无从签发。
        if let Some(pool) = &mcp_pool {
            let inbox =
                peri_acp_types::session::SessionInbox::new(Arc::new(session.queue().clone()));
            pool.bind_agent_session(
                &session_id,
                inbox.handle(),
                session.queue().clone(),
                task_manager.clone(),
            );
        }
        // child 已落库、运行实例已建立：从第一次 await（初始指令入队）起，所有退出
        // 路径都必须结算工作意图，否则父会话进入时会把它当作待续跑候选。显式路径
        // await `settle`，future 被 drop 时由 Drop 先停后台工作再兜底结算。
        let mut child_settlement = execution.settlement(Arc::clone(&session));
        let v2_ctx = ctx_builder.build(
            Some(session),
            llm,
            Arc::clone(&chain),
            tools_arc,
            &self.ctx.cwd,
            cancel_token.clone(),
            Some(self.ctx.middleware_factory.build_tool_resolver()),
            compact_config,
            context_budget,
            compact_llm,
            Some(crate::session::subagent::agent_id_from_child_thread(
                &session_id,
            )),
        );

        // EventBus forwarder（v2 → v1 ExecutorEvent，转发给 event_handler）。
        // 经注入的 ForwarderLauncherFn 启动——biased select 顺序不变量单点
        // 保持在 ACP `crate::event::spawn_eventbus_forwarder`（与主 executor
        // 调用点一致）。
        //
        // 事件三层化（3.0 M-event-chain）：workflow agent 的 v2 事件同时经
        // 注入的 publish_hook（`Controller::publish_event`）统一发射（事件
        // 统一出口；主 session 的事件泵按 session_id 过滤消费，workflow agent
        // 流式事件与子 agent 一致进入协议化路径）。内部 handler 保留
        // （Langfuse/usage/progress）。
        let crate::session::subagent::V2SubagentContext {
            context,
            session,
            event_handles,
            agent_id: _,
            event_bus,
        } = v2_ctx;
        // 语义迭代计数归 TurnContext（循环内 advance_step 推进）：循环消费 context
        // 后仍可读，用于 attempt 的真实用量；未进入循环的执行才留 None。
        let turn = Arc::clone(&context.session.turn);
        let handler_for_forwarder = Arc::clone(&event_handler);
        let publish_hook = self.ctx.publish_hook.clone();
        let sid_for_forwarder = self.ctx.session_id.clone();
        let forwarder_handle = (self.ctx.forwarder_launcher)(
            event_handles,
            sid_for_forwarder.clone().unwrap_or_default(),
            Box::new(move |source, mut exec_ev| {
                crate::agent::subagent_event_forwarder::set_source_agent_id(
                    &mut exec_ev,
                    &source.agent_id,
                );
                handler_for_forwarder.on_event(exec_ev.clone());
                if let (Some(hook), Some(sid)) = (publish_hook.as_ref(), sid_for_forwarder.as_ref())
                {
                    hook(sid, &source, &exec_ev);
                }
            }),
        );

        // push prompt 到 queue
        let assignment_prompt = super::report::assignment_prompt(&params.prompt);
        if let Err(detail) =
            enqueue_workflow_initial_prompt(session.queue(), &assignment_prompt).await
        {
            // 没有接纳到初始指令的任务不得继续跑：先关闭本次会话作用域与转发器，
            // 再把失败原样交给工作流执行器（任务终态为失败）。
            drop(context);
            let _ = await_workflow_forwarder(event_bus, forwarder_handle).await;
            let scope_closed = close_session_scope(&session, &session_id).await;
            child_settlement.settle(AgentStatus::Error).await;
            return initial_prompt_enqueue_failure(detail, started_at, &observation)
                .after_scope_close(scope_closed);
        }

        let mut execution_guard = match peri_acp_types::tasks::TaskManager::begin_external_execution(
            task_manager.as_ref(),
            "workflow-agent",
        ) {
            Ok(guard) => guard,
            Err(detail) => {
                drop(context);
                let _ = await_workflow_forwarder(event_bus, forwarder_handle).await;
                let scope_closed = close_session_scope(&session, &session_id).await;
                child_settlement.settle(AgentStatus::Error).await;
                return AgentRunResult::Dead {
                    scope_close_incomplete: false,
                    reason: Some("execution-blocked".into()),
                    detail: Some(detail),
                    duration_ms: Some(started_at.elapsed().as_millis() as u64),
                    token_count: observation.snapshot().reported_output_tokens(),
                }
                .after_scope_close(scope_closed);
            }
        };
        // 7. 运行 v2 ReAct 循环
        let mut loop_result = crate::agent::stages::run_react_loop(context, max_iterations).await;
        if let Err(error) = crate::session::subagent::flush_session_history(&session).await {
            loop_result = crate::agent::stages::LoopResult::Error(error);
        }
        let forwarder_result = await_workflow_forwarder(event_bus, forwarder_handle).await;
        let forwarded = forwarder_result.is_ok();
        let projected = result::project_run_result(
            loop_result,
            forwarder_result,
            result::ProjectionInputs {
                session: &session,
                observation: &observation,
                params: &params,
                effective_model: &model_name,
                // 上限取执行器解析值，用量取循环自己的语义迭代计数
                // （`advance_step` 与预算检查同点推进）。
                execution_limits: peri_acp_types::workflow::AgentExecutionLimits {
                    iteration_limit: max_iterations as u64,
                    iteration_limit_source,
                    iterations_used: Some(turn.current_step() as u64),
                },
                started_at,
            },
        );
        // 结算路径（宿主）按 (run_id, agent_id) 读取本次解析结果；run 结束时回收。
        self.record_execution_limits(&params.run_id, params.agent_id, projected.execution_limits);
        if forwarded {
            execution_guard.confirm_stopped();
        }

        // 外部执行 guard 持有的 tracker token 只有 drop 才释放；若它活到函数返回，
        // `close_session_scope` 会等在同一 tracker 上直到 `ExecutionScope::wait` 的
        // 5 秒预算耗尽，把每次正常 run 的结清都记成失败（并按 scope 数逐次累加）。
        // 未确认执行仍由 `ExternalGuard::drop` 写入 uncertainty 记录，不掩盖证据缺失。
        drop(execution_guard);

        // Scope-close uncertainty overrides the callback outcome before handoff.
        // Still settle the child to Error so it is not repeatedly recovered as
        // active work. A child-metadata persistence error is logged separately.
        let scope_closed = close_session_scope(&session, &session_id).await;
        let projected = projected.after_scope_close(scope_closed);
        child_settlement
            .settle(result::terminal_status_after_close(
                &projected.result,
                scope_closed,
            ))
            .await;

        // Telemetry and the workflow callback must agree on an incomplete close.
        if let Some(ref hooks) = self.ctx.langfuse_hooks {
            let handle = (hooks.on_turn_end)(projected.telemetry_outcome());
            drop(handle);
        }

        projected.result
    }

    fn execution_limits(
        &self,
        run_id: &str,
        agent_id: u64,
    ) -> Option<peri_acp_types::workflow::AgentExecutionLimits> {
        self.execution_limits
            .read()
            .get(&(run_id.to_string(), agent_id))
            .copied()
    }

    fn forget_run(&self, run_id: &str) {
        self.execution_limits
            .write()
            .retain(|(run, _), _| run != run_id);
    }
}

/// 启动 workflow agent 前的初始指令入队。
///
/// 接纳失败必须让本次任务失败：没有初始指令的运行只会走完空循环并被报告为成功。
pub(super) async fn enqueue_workflow_initial_prompt(
    queue: &crate::session::MessageQueue,
    prompt: &str,
) -> Result<(), String> {
    queue
        .enqueue_sent(QueuedMessage::new(
            MessageKind::Prompt,
            MessageSource::UserInput,
            BaseMessage::human(prompt.to_owned()),
        ))
        .await
        .map(|_| ())
        .map_err(|error| {
            warn!(%error, "workflow agent prompt enqueue failed");
            format!("workflow initial prompt could not be admitted to the thread MQ: {error}")
        })
}

/// 初始指令未接纳时的任务终态：失败，绝不继续运行。
pub(super) fn initial_prompt_enqueue_failure(
    detail: String,
    started_at: std::time::Instant,
    observation: &crate::agent::workflow::agent::observation::WorkflowObservation,
) -> peri_acp_types::workflow::AgentRunResult {
    peri_acp_types::workflow::AgentRunResult::Dead {
        scope_close_incomplete: false,
        reason: Some("initial-prompt-enqueue-failed".into()),
        detail: Some(detail),
        duration_ms: Some(started_at.elapsed().as_millis() as u64),
        token_count: observation.snapshot().reported_output_tokens(),
    }
}

/// 结清 workflow 子会话作用域（幂等）。
///
/// workflow agent 的 session 使用独立取消令牌，父会话关闭不会级联到它；
/// 若不在本 run 收尾时结清，其 workspace 任务作用域与外部执行证据会一直
/// 保留在会话 MCP 池绑定中，父会话关闭将被 `verify_environment_close` 以
/// `Incomplete: child ... resources or required messages unfinished` 拒绝
/// （-32010），且父会话停在半关闭态，重试也无法成功。
///
/// 返回是否结清。失败只记录：run 结果已产出，清理不完整不得改写结果语义，
/// 但调用方据此把 child 终态降级为失败，不把未释放资源的执行记成正常完成。
pub(super) async fn close_session_scope(
    session: &Arc<crate::session::Session>,
    session_id: &str,
) -> bool {
    match crate::session::subagent::close_subagent_session_scope(session.clone()).await {
        Ok(()) => true,
        Err(error) => {
            warn!(
                session_id = %session_id,
                %error,
                "workflow agent session scope close incomplete"
            );
            false
        }
    }
}

pub(super) async fn await_workflow_forwarder(
    event_bus: Arc<crate::agent::events_v2::EventBus>,
    handle: tokio::task::JoinHandle<()>,
) -> Result<(), peri_acp_types::session::ExecutionFailure> {
    drop(event_bus);
    handle.await.map_err(|_| {
        peri_acp_types::session::ExecutionFailure::internal(
            "Workflow agent event forwarding failed",
        )
    })
}

/// 工作流与 agent.md 的工具名匹配沿用 subagent 的大小写无关语义。
/// 单独的 `*` 表示保留全部候选工具；随后仍由 disallowedTools 过滤。
pub(super) fn tool_name_in(names: &[String], tool_name: &str) -> bool {
    use peri_acp_types::builtin_mcp;

    // 只按注册表归一 builtin，外部 MCP 工具保留完整身份。两侧都归一，
    // 让旧 agent.md 声明与模型面的 effective name 表达同一能力边界。
    let original = builtin_mcp::original_tool_name_of_effective(tool_name).unwrap_or(tool_name);
    matches!(names, [wildcard] if wildcard == "*")
        || names.iter().any(|name| {
            name.eq_ignore_ascii_case(tool_name)
                || builtin_mcp::original_tool_name_of_effective(name)
                    .unwrap_or(name)
                    .eq_ignore_ascii_case(original)
        })
}

/// 构造 workflow agent 的模型边界（M4）。
///
/// base system 为按 workflow 能力投影后的冻结输入；动态贡献**在每次
/// `ModelRequest` 构造时**从同一条链读取（`before_agent` 已填充缓存），组合
/// 统一走 `combine_system_prompt_with_dynamic`（M1 权威，分隔符与 reserved
/// marker 校验不在此重复实现）。
pub(crate) fn workflow_model_bridge(
    base_model: Arc<dyn peri_model::Model>,
    system_prompt: String,
    external_instructions: Option<Arc<str>>,
    environment_prompt: Option<Arc<str>>,
    legacy_embedded_instructions: bool,
    chain: Arc<MiddlewareChain>,
    session_id: &str,
) -> Box<dyn crate::agent::react::ReactLLM + Send + Sync> {
    let contribution_chain = Arc::clone(&chain);
    Box::new(
        AgentModelBridge::from_arc(base_model)
            .with_system(system_prompt)
            .with_external_instructions(external_instructions)
            .with_environment_prompt(environment_prompt)
            .with_legacy_prompt_provenance(legacy_embedded_instructions, true)
            .with_system_contribution_provider(Arc::new(move || {
                contribution_chain.collect_prompt_contributions()
            }))
            .with_session_id(session_id),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn select_workflow_system_prompt(
    definition: Option<&crate::agent::workflow::factory::WorkflowAgentDefinition>,
    frozen_default: Option<&str>,
    agent_prompt_builder: &crate::agent::workflow::factory::WorkflowAgentPromptBuilder,
    fallback: &crate::agent::workflow::factory::WorkflowSystemPromptFallback,
    cwd: &str,
    date: Option<&str>,
    language: Option<&str>,
    skill_access: SkillToolAccess,
) -> String {
    match definition {
        Some(definition) => agent_prompt_builder(
            definition.prompt_overrides.as_ref(),
            cwd,
            date,
            language,
            skill_access,
        ),
        None => frozen_default
            .filter(|_| skill_access == SkillToolAccess::Full)
            .map(str::to_owned)
            .unwrap_or_else(|| fallback(cwd, date, language, skill_access)),
    }
}

impl Debug for WorkflowAgentContext {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkflowAgentContext")
            .field("cwd", &self.cwd)
            .field("frozen_claude_md_present", &self.frozen_claude_md.is_some())
            .field(
                "frozen_claude_local_md_present",
                &self.frozen_claude_local_md.is_some(),
            )
            .field(
                "frozen_skill_summary_present",
                &self.frozen_skill_summary.is_some(),
            )
            .field(
                "mcp_skill_registry_present",
                &self.mcp_skill_registry.is_some(),
            )
            .field("session_id_present", &self.session_id.is_some())
            .field(
                "session_resources_present",
                &self.session_resources.is_some(),
            )
            .field("compact_config_present", &self.compact_config.is_some())
            .field("cancel_present", &self.cancel.is_some())
            .field("system_prompt_present", &self.system_prompt.is_some())
            .field(
                "external_instructions_present",
                &self.external_instructions.is_some(),
            )
            .field(
                "environment_prompt_present",
                &self.environment_prompt.is_some(),
            )
            .field(
                "legacy_embedded_instructions",
                &self.legacy_embedded_instructions,
            )
            .field("broker_present", &self.broker.is_some())
            .field("permission_mode_present", &self.permission_mode.is_some())
            .field("frozen_date_present", &self.frozen_date.is_some())
            .field("frozen_language_present", &self.frozen_language.is_some())
            .field("progress_tx_present", &self.progress_tx.is_some())
            .field(
                "subagent_ctx_builder_present",
                &self.subagent_ctx_builder.is_some(),
            )
            .field("publish_hook_present", &self.publish_hook.is_some())
            .field("langfuse_hooks_present", &self.langfuse_hooks.is_some())
            .field(
                "langfuse_event_handler_present",
                &self.langfuse_event_handler.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl Debug for WorkflowAgentExecutor {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkflowAgentExecutor")
            .finish_non_exhaustive()
    }
}
