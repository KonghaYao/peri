//! Workflow agent 执行体（p1-wa 归位：自 `peri-acp::host::workflow_agent` 迁入）。
//!
//! 当 Node workflow engine 调用 agent(prompt) 时，`WorkflowRunner` 经
//! [`AgentExecutor`] trait 回调本模块，通过 `build_v2_subagent_context` +
//! `run_react_loop` 执行并返回结果。
//!
//! 复用 SubAgent v2 基础设施：workflow agent 携带 frozen CLAUDE.md / skills
//! 并经过完整中间件链（Filesystem/Terminal/Web），+ error_suggest wiring。
//!
//! # 依赖反转（p1-wa 收口）
//!
//! §0 边 8（Agent 禁入 Middleware/Controller）：执行体所需的 ACP/Controller/
//! Middleware 特有构造全部经注入面参数化（`WorkflowAgentContext` 字段）：
//!
//! - 模型构造（provider alias 解析 / AgentPool 缓存 / retry observer 烘焙）
//!   → [`WorkflowModelFactory`]（ACP 宿主构造）
//! - 中间件链 / 工具 / error_suggest / tool resolver 装配
//!   → [`WorkflowMiddlewareFactory`]（peri-middlewares 实现，ACP 宿主注入）
//! - system prompt fallback 渲染 → [`WorkflowSystemPromptFallback`]（ACP 宿主）
//! - EventBus forwarder 启动（v2 → v1 映射 + biased select 不变量单点）
//!   → `ForwarderLauncherFn`（ACP 宿主构造）
//! - 事件发射（`Controller::publish_event` 统一出口）→ [`WorkflowPublishHook`]
//! - Langfuse 观测（turn 钩子 + 事件旁路）→ `LangfuseHooks` / 事件处理闭包
//!
//! 迁移前 `create_session_workflow_middleware`（session 级 WorkflowMiddleware
//! 装配编排）保留在 ACP 装配面（`host/workflow_agent.rs` 薄壳），本模块只
//! 承载执行单元。

use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::{
    compact::CompactConfig,
    event::{AgentEventHandler, ExecutorEvent, FnEventHandler},
    interaction::UserInteractionBroker,
    messages::BaseMessage,
    session::{MessageKind, MessageSource, QueuedMessage},
    workflow::{AgentExecutor, AgentRunParams, AgentRunResult, ProgressEvent, Usage},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::factory::{
    WorkflowAgentPromptBuilder, WorkflowMiddlewareFactory, WorkflowModelFactory,
    WorkflowPublishHook, WorkflowSystemPromptFallback,
};
use crate::agent::{
    model_bridge::AgentModelBridge,
    stages::{run_react_loop, LoopResult},
    token::ContextBudget,
};
use crate::middleware::chain::MiddlewareChain;
use crate::session::{
    exec::executor::LangfuseHooks,
    exec::executor_helpers::ForwarderLauncherFn,
    subagent::{DefaultSubagentV2ContextBuilder, SubagentV2ContextBuilder},
};

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

    /// Session ID（用于 compact 事件和日志）
    pub session_id: Option<String>,
    /// Compact 配置（None = 不启用自动 compact）
    pub compact_config: Option<CompactConfig>,
    /// 取消令牌（None = workflow agent 创建内部 token）
    pub cancel: Option<CancellationToken>,

    /// 标准 system prompt（session/new 时冻结的 build_system_prompt() 输出）。
    /// None = 回退到注入的 [`WorkflowSystemPromptFallback`] 运行时构建。
    pub system_prompt: Option<String>,
    /// HITL broker + 共享权限模式。两者均 Some 时启用审批；
    /// 任一为 None 时 Bypass（自主后台 agent 默认行为）。
    pub broker: Option<Arc<dyn UserInteractionBroker>>,
    pub permission_mode: Option<Arc<peri_acp_types::permission::SharedPermissionMode>>,

    /// Frozen date + language（system prompt fallback 构建时的日期/语言一致性）。
    pub frozen_date: Option<String>,
    pub frozen_language: Option<String>,

    /// ThreadStore（持久化 workflow agent 消息到统一存储）。
    /// None = 不持久化（内存中运行，当前行为）。
    pub thread_store: Option<Arc<dyn peri_acp_types::store::ThreadStore>>,

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
    pub middleware_factory: Arc<dyn WorkflowMiddlewareFactory>,
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
}

/// Workflow agent executor — builds and runs v2 stages for workflow agent() calls.
pub struct WorkflowAgentExecutor {
    ctx: WorkflowAgentContext,
}

impl WorkflowAgentExecutor {
    pub fn new(ctx: WorkflowAgentContext) -> Self {
        Self { ctx }
    }
}

/// 创建携带 frozen data 的 workflow agent executor。
pub fn create_executor(ctx: WorkflowAgentContext) -> Arc<dyn AgentExecutor> {
    Arc::new(WorkflowAgentExecutor::new(ctx))
}

/// 便捷工厂：创建无 frozen data 的 workflow agent executor。
///
/// p1-wa 备注：调用方均已迁至注入面构造（`host/prompt.rs` /
/// `host/stdio/session/prompt_exec.rs` 直构 `WorkflowAgentContext`），
/// 本函数为 API 兼容保留（dead code 候选，删除留待 API 冻结窗口）。
pub fn create_default_executor(
    model_factory: WorkflowModelFactory,
    middleware_factory: Arc<dyn WorkflowMiddlewareFactory>,
    system_prompt_fallback: WorkflowSystemPromptFallback,
    forwarder_launcher: ForwarderLauncherFn,
    cwd: &str,
) -> Arc<dyn AgentExecutor> {
    Arc::new(WorkflowAgentExecutor::new(WorkflowAgentContext {
        cwd: cwd.to_string(),
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        session_id: None,
        compact_config: None,
        cancel: None,
        system_prompt: None,
        broker: None,
        permission_mode: None,
        frozen_date: None,
        frozen_language: None,
        thread_store: None,
        progress_tx: None,
        subagent_ctx_builder: None,
        agent_prompt_builder: Arc::new(|_, _, _, _| String::new()),
        model_factory,
        middleware_factory,
        system_prompt_fallback,
        forwarder_launcher,
        publish_hook: None,
        langfuse_hooks: None,
        langfuse_event_handler: None,
    }))
}

fn requested_model<'a>(
    request_model: Option<&'a str>,
    agent_definition: Option<&'a super::factory::WorkflowAgentDefinition>,
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

        let started_at = std::time::Instant::now();

        let agent_definition = match params.agent_type.as_deref() {
            Some(agent_type) => match self
                .ctx
                .middleware_factory
                .resolve_agent_definition(agent_type, &self.ctx.cwd)
            {
                Ok(definition) => Some(definition),
                Err(detail) => {
                    warn!(agent_type, %detail, "workflow agent: invalid agent type");
                    return AgentRunResult::Dead {
                        reason: Some("invalid-agent-type".into()),
                        detail: Some(detail),
                    };
                }
            },
            None => None,
        };
        if params.max_tokens == Some(0) {
            return AgentRunResult::Dead {
                reason: Some("invalid-max-tokens".into()),
                detail: Some("maxTokens must be greater than zero".into()),
            };
        }

        // 请求的 model 显式覆盖 agent definition；空值 / inherit 表示使用父 provider。
        let requested_model = requested_model(params.model.as_deref(), agent_definition.as_ref());

        // 0. GAP-08: Langfuse turn 开始钩子（注入面；迁移前 `langfuse_session`
        // 恒 None 未接线，None = 遥测禁用）。
        if let Some(ref hooks) = self.ctx.langfuse_hooks {
            (hooks.on_turn_start)(&params.prompt);
        }

        // Agent usage 累积器：从 LlmCallEnd 事件收集实际 token 用量
        // (output_tokens, model_name)
        let usage_stats: Arc<Mutex<(u64, Option<String>)>> = Arc::new(Mutex::new((0, None)));
        let usage_stats_for_handler = Arc::clone(&usage_stats);

        // 工具调用次数计数器
        let tool_call_count: std::sync::Arc<std::sync::Mutex<u64>> =
            std::sync::Arc::new(std::sync::Mutex::new(0));
        let tool_call_count_for_handler = Arc::clone(&tool_call_count);
        let progress_tx_for_handler = self.ctx.progress_tx.clone();
        let run_id_for_handler = params.run_id.clone();
        let agent_id_for_handler = params.agent_id;
        let langfuse_event_handler = self.ctx.langfuse_event_handler.clone();

        let event_handler: Arc<dyn AgentEventHandler> = Arc::new(FnEventHandler(
            move |event: ExecutorEvent| {
                match &event {
                    ExecutorEvent::ToolStart { name, .. } => {
                        *tool_call_count_for_handler.lock().unwrap() += 1;
                        debug!(tool = %name, "workflow agent: tool started");
                        // 发送实时进度更新
                        if let Some(ref tx) = progress_tx_for_handler {
                            let s = usage_stats_for_handler.lock();
                            let tc = tool_call_count_for_handler.lock().unwrap();
                            if let Err(e) = tx.send(ProgressEvent::AgentProgress {
                                run_id: run_id_for_handler.clone(),
                                agent_id: agent_id_for_handler,
                                label: None,
                                phase: None,
                                model: None,
                                model_tier: None,
                                token_count: Some(s.0),
                                tool_count: Some(*tc),
                            }) {
                                warn!(target: "workflow", run_id = %run_id_for_handler, agent_id = agent_id_for_handler, error = %e, "progress_tx.send failed (ToolStart)");
                            }
                        }
                    }
                    ExecutorEvent::ToolEnd { name, is_error, .. } => {
                        if *is_error {
                            warn!(tool = %name, "workflow agent: tool failed");
                        } else {
                            debug!(tool = %name, "workflow agent: tool completed");
                        }
                    }
                    ExecutorEvent::LlmCallEnd { model, usage, .. } => {
                        debug!(
                            model = %model,
                            tokens = ?usage.as_ref().map(|u| (u.input_tokens, u.output_tokens)),
                            "workflow agent: llm call completed"
                        );
                        // 累积真实 token 用量，供 AgentRunResult 上报
                        {
                            let mut s = usage_stats_for_handler.lock();
                            if let Some(u) = usage {
                                s.0 += u.output_tokens as u64;
                            }
                            s.1 = Some(model.clone());
                        }
                        // 发送实时进度更新
                        if let Some(ref tx) = progress_tx_for_handler {
                            let s = usage_stats_for_handler.lock();
                            let tc = tool_call_count_for_handler.lock().unwrap();
                            if let Err(e) = tx.send(ProgressEvent::AgentProgress {
                                run_id: run_id_for_handler.clone(),
                                agent_id: agent_id_for_handler,
                                label: None,
                                phase: None,
                                model: None,
                                model_tier: None,
                                token_count: Some(s.0),
                                tool_count: Some(*tc),
                            }) {
                                warn!(target: "workflow", run_id = %run_id_for_handler, agent_id = agent_id_for_handler, error = %e, "progress_tx.send failed (LlmCallEnd)");
                            }
                        }
                    }
                    ExecutorEvent::LlmRetrying {
                        attempt,
                        max_attempts,
                        error,
                        ..
                    } => {
                        warn!(attempt, max_attempts, error = %error, "workflow agent: llm retrying");
                    }
                    _ => {}
                }

                // Langfuse 事件转发（注入的观测旁路处理器；`UnifiedLangfuseEvent`
                // 映射与 bridge 构造收 ACP 宿主，边 8 禁止本层直引 Controller）
                if let Some(ref f) = langfuse_event_handler {
                    f(&event);
                }
            },
        ));

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

        // 模型解析完成后尽早上报有效模型名（模型信息专用更新）：TUI 在
        // 运行中即可显示 Model 列，不必等首个 LlmCallEnd。计数保持 None，避免
        // 引擎重试同一 agent 时以 0 覆盖前一次尝试已累计的统计。
        // reducer 仅在 Some 时覆盖 agent.model，后续 ToolStart/LlmCallEnd
        // 进度（model: None）不会冲掉该值。
        if let Some(ref tx) = self.ctx.progress_tx {
            if let Err(e) = tx.send(ProgressEvent::AgentProgress {
                run_id: params.run_id.clone(),
                agent_id: params.agent_id,
                label: None,
                phase: None,
                model: Some(model_name.clone()),
                model_tier: model_tier.clone(),
                token_count: None,
                tool_count: None,
            }) {
                warn!(target: "workflow", run_id = %params.run_id, agent_id = params.agent_id, error = %e, "progress_tx.send failed (model)");
            }
        }

        // 2. 注册工具（端口装配：fs/terminal/web/skills tools，仅 project-level
        // skills——workflow agent 无 plugin_skill_roots）。
        let mut tools = self.ctx.middleware_factory.build_tools(&self.ctx.cwd);

        // 3. agent definition 工具边界优先，再叠加 workflow allowedTools。
        if let Some(definition) = agent_definition.as_ref() {
            if let Some(allowed) = definition.allowed_tools.as_ref() {
                tools.retain(|tool| tool_name_in(allowed, tool.name()));
            }
            if !definition.disallowed_tools.is_empty() {
                tools.retain(|tool| !tool_name_in(&definition.disallowed_tools, tool.name()));
            }
            if !definition.allowed_write_dirs.is_empty()
                && definition
                    .allowed_tools
                    .as_ref()
                    .is_none_or(|allowed| !allowed.is_empty())
                && !tool_name_in(&definition.disallowed_tools, "SandboxWrite")
            {
                if let Some(sandbox_write) = self
                    .ctx
                    .middleware_factory
                    .build_sandbox_write_tool(&self.ctx.cwd, &definition.allowed_write_dirs)
                {
                    tools.push(sandbox_write);
                }
            }
        }
        if let Some(allowed) = params
            .allowed_tools
            .as_ref()
            .filter(|allowed| !allowed.is_empty())
        {
            tools.retain(|tool| tool_name_in(allowed, tool.name()));
        }

        // 4. 指定 agent type 时按相同的 subagent overrides 渲染 prompt；否则
        // 继续复用 session 冻结的默认 subagent prompt。
        let system_prompt = if let Some(definition) = agent_definition.as_ref() {
            (self.ctx.agent_prompt_builder)(
                definition.prompt_overrides.as_ref(),
                &self.ctx.cwd,
                self.ctx.frozen_date.as_deref(),
                self.ctx.frozen_language.as_deref(),
            )
        } else {
            self.ctx.system_prompt.clone().unwrap_or_else(|| {
                (self.ctx.system_prompt_fallback)(
                    &self.ctx.cwd,
                    self.ctx.frozen_date.as_deref(),
                    self.ctx.frozen_language.as_deref(),
                )
            })
        };

        // 5. 构建中间件链（端口装配；frozen data / HITL 语义自 ctx 读取）
        let mut chain = MiddlewareChain::new();
        for mw in self.ctx.middleware_factory.build_middlewares(
            &self.ctx,
            &model_name,
            agent_definition
                .as_ref()
                .map(|definition| definition.skill_names.as_slice())
                .unwrap_or_default(),
        ) {
            chain.add(mw);
        }

        // 6. v2 stages 装配（替代 SubAgentBuilder）
        let cancel_token = self.ctx.cancel.clone().unwrap_or_default();
        let max_iterations = agent_definition
            .as_ref()
            .map(|definition| definition.max_iterations)
            .filter(|max_iterations| *max_iterations > 0)
            .unwrap_or(200);

        // tools: Vec<Box<dyn BaseTool>> → Vec<Arc<dyn BaseTool>>
        let tools_arc: Vec<Arc<dyn crate::tools::BaseTool>> = tools
            .into_iter()
            .map(|t| Arc::from(t) as Arc<dyn crate::tools::BaseTool>)
            .collect();

        // 收集中间件 prompt_contribution，合并到 system_prompt
        let contributions = chain.collect_prompt_contributions();
        let system_prompt = if contributions.is_empty() {
            system_prompt
        } else {
            format!("{system_prompt}\n\n{contributions}")
        };

        // 构造 AgentModelBridge（现在 system_prompt 已就绪）
        let mut base_llm =
            AgentModelBridge::from_arc(base_model).with_system(system_prompt.clone());
        if let Some(ref sid) = self.ctx.session_id {
            base_llm = base_llm.with_session_id(sid);
        }
        let llm: Box<dyn crate::agent::react::ReactLLM + Send + Sync> = Box::new(base_llm);

        // error_suggest wiring（与 SubAgentBuilder.with_error_suggest() 等价；
        // .claude/agents/ 目录存在性检查在端口实现内）
        let all_tool_names: Vec<String> = tools_arc.iter().map(|t| t.name().to_string()).collect();
        let (error_suggest_registry, snapshot) = self
            .ctx
            .middleware_factory
            .build_error_suggest(&self.ctx.cwd, &all_tool_names);

        // 构造 v2 StageContext（workflow agent 无 parent_messages）
        // agent_id=None：workflow 无 child_thread_id，内部 AgentId::new() 兜底（C1）
        let ctx_builder = self
            .ctx
            .subagent_ctx_builder
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultSubagentV2ContextBuilder));
        let v2_ctx = ctx_builder.build(
            None, // workflow agent 无预创建 session（内部自建）
            llm,
            chain,
            tools_arc,
            &self.ctx.cwd,
            cancel_token.clone(),
            Some(self.ctx.middleware_factory.build_tool_resolver()),
            Some(error_suggest_registry),
            Some(snapshot),
            compact_config,
            context_budget,
            compact_llm,
            None, // workflow 无 child_thread_id，内部 AgentId::new() 兜底（C1）
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
        let handler_for_forwarder = Arc::clone(&event_handler);
        let publish_hook = self.ctx.publish_hook.clone();
        let sid_for_forwarder = self.ctx.session_id.clone();
        (self.ctx.forwarder_launcher)(
            v2_ctx.event_handles,
            sid_for_forwarder.clone().unwrap_or_default(),
            Box::new(move |source, exec_ev| {
                handler_for_forwarder.on_event(exec_ev.clone());
                if let (Some(hook), Some(sid)) = (publish_hook.as_ref(), sid_for_forwarder.as_ref())
                {
                    hook(sid, &source, &exec_ev);
                }
            }),
        );

        // push prompt 到 queue
        v2_ctx.context.session.queue.push(QueuedMessage::new(
            MessageKind::Prompt,
            MessageSource::UserInput,
            BaseMessage::human(params.prompt.clone()),
        ));

        // 7. 运行 v2 ReAct 循环
        let loop_result = run_react_loop(v2_ctx.context, max_iterations).await;

        let agent_result = match loop_result {
            LoopResult::Completed => {
                let output_text = crate::session::subagent::extract_last_ai_text(&v2_ctx.session);

                // 获取 agent 执行期间累积的 token 用量
                let (total_output_tokens, last_model) = {
                    let s = usage_stats.lock();
                    let mut tokens = s.0;
                    // P0 fallback: haiku 等模型 usage=None 时 token 累积为 0，
                    // 按 output_text 长度启发式估算（每个 token ~4 字符）
                    if tokens == 0 && !output_text.is_empty() {
                        tokens = (output_text.len() as u64 / 4).max(1);
                    }
                    // 如果事件从未 emit LlmCallEnd（如纯工具调用），仍应回传模型
                    // 工厂解析后的有效模型名，而非 workflow 脚本中的 alias。
                    let model = reported_model(s.1.clone(), &model_name);
                    (tokens, model)
                };

                // Schema 校验
                if let Some(ref schema) = params.schema {
                    if let Err(err) = validate_json_schema(&output_text, schema) {
                        debug!(error = %err, "Workflow agent: schema validation failed");
                        AgentRunResult::Dead {
                            reason: Some("no-structured-output".into()),
                            detail: Some(err),
                        }
                    } else {
                        AgentRunResult::Ok {
                            output: serde_json::Value::String(output_text),
                            usage: Usage {
                                output_tokens: total_output_tokens,
                            },
                            model: last_model,
                            tool_count: {
                                let c = tool_call_count.lock().unwrap();
                                Some(*c)
                            },
                            token_count: Some(total_output_tokens),
                            phase: params.phase.clone(),
                            duration_ms: Some(started_at.elapsed().as_millis() as u64),
                        }
                    }
                } else {
                    AgentRunResult::Ok {
                        output: serde_json::Value::String(output_text),
                        usage: Usage {
                            output_tokens: total_output_tokens,
                        },
                        model: last_model,
                        tool_count: {
                            let c = tool_call_count.lock().unwrap();
                            Some(*c)
                        },
                        token_count: Some(total_output_tokens),
                        phase: params.phase.clone(),
                        duration_ms: Some(started_at.elapsed().as_millis() as u64),
                    }
                }
            }
            LoopResult::Interrupted => {
                debug!("Workflow agent: execution interrupted");
                AgentRunResult::Dead {
                    reason: Some("interrupted".into()),
                    detail: Some("Workflow agent execution was interrupted".into()),
                }
            }
            LoopResult::Error(e) => {
                debug!(error = %e, "Workflow agent: execution failed");
                AgentRunResult::Dead {
                    reason: Some("runagent-threw".into()),
                    detail: Some(e.to_string()),
                }
            }
        };

        // GAP-08: 结束 Langfuse trace（fire-and-forget flush；注入钩子）
        if let Some(ref hooks) = self.ctx.langfuse_hooks {
            let error_output = match &agent_result {
                AgentRunResult::Dead { detail, .. } => detail.clone(),
                _ => None,
            };
            let handle = (hooks.on_turn_end)(error_output);
            drop(handle); // fire-and-forget flush
        }

        agent_result
    }
}

/// 工作流与 agent.md 的工具名匹配沿用 subagent 的大小写无关语义。
/// 单独的 `*` 表示保留全部候选工具；随后仍由 disallowedTools 过滤。
fn tool_name_in(names: &[String], tool_name: &str) -> bool {
    matches!(names, [wildcard] if wildcard == "*")
        || names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(tool_name))
}

fn reported_model(last_model: Option<String>, effective_model: &str) -> Option<String> {
    last_model.or_else(|| Some(effective_model.to_string()))
}

/// JSON Schema 校验——基础类型 + required 字段检查。
///
/// schema 为 None 或空 {} 时仅验证是合法 JSON（向后兼容）。
/// 否则检查：
/// 1. 顶层 type 匹配（object/array/string/number/boolean/null）
/// 2. 若 type 为 object，检查 required 字段存在
/// 3. 若 type 为 object 且有 properties，检查各属性 type 匹配
fn validate_json_schema(text: &str, schema: &serde_json::Value) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("output is not valid JSON: {e}"))?;

    // 如果 schema 为空或不是 object，仅验证 JSON 格式
    let schema_obj = match schema.as_object() {
        Some(obj) if obj.is_empty() => return Ok(()),
        Some(_) => schema,
        _ => return Ok(()),
    };

    // 检查顶层 type
    if let Some(expected_type) = schema_obj.get("type").and_then(|v| v.as_str()) {
        let actual_type = json_type_name(&value);
        if actual_type != expected_type {
            return Err(format!(
                "expected top-level type '{expected_type}', got '{actual_type}'"
            ));
        }
    }

    // 对 object 类型检查 required + properties
    if let Some(obj) = value.as_object() {
        if let Some(required) = schema_obj.get("required").and_then(|v| v.as_array()) {
            for field in required {
                let field_name = field
                    .as_str()
                    .ok_or_else(|| format!("required 数组元素不是字符串: {field}"))?;
                if !obj.contains_key(field_name) {
                    return Err(format!("missing required field: {field_name}"));
                }
            }
        }

        if let Some(properties) = schema_obj.get("properties").and_then(|v| v.as_object()) {
            for (prop_name, prop_schema) in properties {
                if let Some(prop_value) = obj.get(prop_name) {
                    if let Some(expected_type) = prop_schema.get("type").and_then(|v| v.as_str()) {
                        let actual_type = json_type_name(prop_value);
                        if actual_type != expected_type {
                            return Err(format!(
                                "field '{prop_name}': expected type '{expected_type}', got '{actual_type}'"
                            ));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// 返回 JSON value 的类型名称（用于错误消息）。
fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::{reported_model, requested_model, tool_name_in};
    use crate::agent::workflow::WorkflowAgentDefinition;

    #[test]
    fn agent_type_tool_matching_is_case_insensitive() {
        assert!(tool_name_in(&["Read".into(), "Grep".into()], "read"));
        assert!(tool_name_in(&["*".into()], "Write"));
        assert!(!tool_name_in(&["Read".into()], "Write"));
    }

    #[test]
    fn requested_model_prefers_workflow_value() {
        let definition = WorkflowAgentDefinition {
            model: Some("haiku".into()),
            ..Default::default()
        };

        assert_eq!(
            requested_model(Some("sonnet"), Some(&definition)),
            Some("sonnet")
        );
    }

    #[test]
    fn requested_model_inherit_overrides_agent_definition() {
        let definition = WorkflowAgentDefinition {
            model: Some("haiku".into()),
            ..Default::default()
        };

        assert_eq!(requested_model(Some("inherit"), Some(&definition)), None);
    }

    #[test]
    fn requested_model_trims_concrete_model_name() {
        assert_eq!(
            requested_model(Some("  claude-sonnet-4-5  "), None),
            Some("claude-sonnet-4-5")
        );
    }

    #[test]
    fn requested_model_uses_agent_definition_when_omitted() {
        let definition = WorkflowAgentDefinition {
            model: Some("haiku".into()),
            ..Default::default()
        };

        assert_eq!(requested_model(None, Some(&definition)), Some("haiku"));
    }

    #[test]
    fn result_model_falls_back_to_effective_model() {
        assert_eq!(
            reported_model(None, "claude-haiku-4-5"),
            Some("claude-haiku-4-5".into())
        );
        assert_eq!(
            reported_model(Some("provider-reported".into()), "claude-haiku-4-5"),
            Some("provider-reported".into())
        );
    }
}
