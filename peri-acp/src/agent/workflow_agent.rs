//! Workflow agent executor — bridges workflow engine's agent() calls to v2 stages.
//!
//! 当 Node workflow engine 调用 agent(prompt) 时，
//! WorkflowRunner 通过 AgentExecutor trait 回调此模块，
//! 通过 `build_v2_subagent_context` + `run_react_loop` 执行并返回结果。
//!
//! 复用 SubAgent v2 基础设施：workflow agent 携带
//! frozen CLAUDE.md / skills 并经过完整中间件链（Filesystem/Terminal/Web），
//! + error_suggest wiring。

use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use peri_agent::{
    agent::{
        compact_v2::CompactConfig,
        events::{AgentEventHandler, ExecutorEvent, FnEventHandler},
        model_bridge::AgentModelBridge,
        token::ContextBudget,
        AgentCancellationToken,
    },
    interaction::UserInteractionBroker,
};
use peri_middlewares::middleware::TodoMiddleware;
use peri_middlewares::{
    middleware::{FilesystemMiddleware, TerminalMiddleware, WebMiddleware},
    prelude::*,
    tools::TodoItem,
};
use peri_workflow::protocol::{AgentRunParams, AgentRunResult, ProgressEvent, Usage};
use peri_workflow::runner::AgentExecutor;
use tracing::{debug, warn};

use crate::provider::LlmProvider;
use crate::session::agent_pool::AgentPool;

/// Workflow agent 构建上下文——携带 session 级 frozen data。
///
/// frozen 数据在 session/new 时捕获，确保 workflow agent 看到的
/// CLAUDE.md / skills 与主会话一致（系统提示词稳定性第一优先级）。
#[derive(Clone)]
pub struct WorkflowAgentContext {
    /// LLM provider——通过 Arc<RwLock<>> 共享，provider/model 切换后自动感知，无需重建 executor。
    pub provider: Arc<RwLock<LlmProvider>>,
    pub cwd: String,
    /// Frozen CLAUDE.md content（含解析的 @import），None = 无文件。
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content，None = 无文件。
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary，None = 无 skills。
    pub frozen_skill_summary: Option<String>,

    // Phase 2 新增
    /// Session ID（用于 compact 事件和日志）
    pub session_id: Option<String>,
    /// Compact 配置（None = 不启用自动 compact）
    pub compact_config: Option<CompactConfig>,
    /// 取消令牌（None = workflow agent 创建内部 token）
    pub cancel: Option<AgentCancellationToken>,

    // GAP-05: 标准 system prompt（session/new 时冻结的 build_system_prompt() 输出）。
    // None = 回退到 build_system_prompt() 运行时构建。
    pub system_prompt: Option<String>,
    // GAP-03: HITL broker + 共享权限模式。两者均 Some 时启用审批；
    // 任一为 None 时 Bypass（自主后台 agent 默认行为）。
    pub broker: Option<Arc<dyn UserInteractionBroker>>,
    pub permission_mode: Option<Arc<SharedPermissionMode>>,

    // GAP-16: Frozen date + language（用于 system prompt fallback 构建时的日期/语言一致性）。
    pub frozen_date: Option<String>,
    pub frozen_language: Option<String>,

    // GAP-13: AgentPool LLM 缓存（复用 reqwest::Client，~1-2 MB/实例）。
    // None = 每次创建新 Model（当前行为，向后兼容）。
    pub agent_pool: Option<Arc<parking_lot::Mutex<AgentPool>>>,

    /// PeriConfig（用于 model alias 解析，如 haiku/sonnet/opus → 真实模型名）。
    /// None = 不做 alias 解析，model 参数按字面量传给 provider。
    pub peri_config: Option<Arc<crate::provider::PeriConfig>>,

    // GAP-08: Langfuse 追踪会话（None = 不启用遥测）。
    pub langfuse_session: Option<Arc<crate::langfuse::session::LangfuseSession>>,

    // GAP-18: ThreadStore（持久化 workflow agent 消息到统一存储）。
    // None = 不持久化（内存中运行，当前行为）。
    pub thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,

    /// 进度事件发送通道（None = 不发送 agent_progress 事件）
    pub progress_tx:
        Option<tokio::sync::mpsc::UnboundedSender<peri_workflow::protocol::ProgressEvent>>,
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
pub fn create_default_executor(provider: LlmProvider, cwd: &str) -> Arc<dyn AgentExecutor> {
    Arc::new(WorkflowAgentExecutor::new(WorkflowAgentContext {
        provider: Arc::new(RwLock::new(provider)),
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
        agent_pool: None,
        langfuse_session: None,
        thread_store: None,
        peri_config: None,
        progress_tx: None,
    }))
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
            "Workflow agent: starting execution"
        );

        let started_at = std::time::Instant::now();

        // 0. GAP-08: 创建 Langfuse tracer（如果 session 可用）
        let langfuse_tracer = self.ctx.langfuse_session.as_ref().map(|s| {
            let session_clone = Arc::clone(s);
            let config = session_clone.config.clone();
            let session: std::sync::Arc<dyn crate::langfuse::LangfuseSessionLike> = session_clone;
            Arc::new(parking_lot::Mutex::new(
                crate::langfuse::tracer::LangfuseTracer::new(
                    session,
                    self.ctx.session_id.clone().unwrap_or_default(),
                    config,
                ),
            ))
        });
        if let Some(ref tracer) = langfuse_tracer {
            tracer.lock().on_turn_start(&params.prompt);
        }

        // 0b. 创建日志 + Langfuse event handler
        // 合并 3 次 provider.read() 为一次，避免中间切换导致 display_name 与实际 provider 不一致。
        // 用块作用域确保 RwLockReadGuard 在第一个 .await 前释放。
        // 如 workflow 脚本指定了 model 参数：
        //   1) 有 PeriConfig → 尝试 alias 解析（haiku/sonnet/opus → 真实模型名）
        //   2) 解析失败或无 PeriConfig → 替换 provider 的 model name 按字面量使用
        let (provider_display_name, effective_provider) = {
            let provider_read = self.ctx.provider.read();
            let display_name = provider_read.display_name().to_string();
            let effective = if let Some(ref model) = params.model {
                self.ctx
                    .peri_config
                    .as_ref()
                    .and_then(|cfg| LlmProvider::from_config_for_alias(cfg, model))
                    .unwrap_or_else(|| provider_read.with_model_name(model.clone()))
            } else {
                provider_read.clone()
            };
            (display_name, effective)
        };

        // 构造统一 Langfuse 桥接器（替代 forward_langfuse_event）
        let langfuse_bridge: Option<crate::langfuse::bridge::LangfuseBridge> =
            langfuse_tracer.as_ref().map(|t| {
                crate::langfuse::bridge::LangfuseBridge::new(
                    std::sync::Arc::clone(t),
                    provider_display_name.clone(),
                )
            });
        let bridge_for_handler = langfuse_bridge.clone();

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
                                token_count: s.0,
                                tool_count: *tc,
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
                                token_count: s.0,
                                tool_count: *tc,
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

                // Langfuse 事件转发（统一桥接器）
                if let Some(ref bridge) = bridge_for_handler {
                    if let Some(u) =
                        crate::langfuse::bridge::UnifiedLangfuseEvent::from_executor_event(
                            event.clone(),
                        )
                    {
                        let mut dummy_stage = None;
                        bridge.process_event(&u, &mut dummy_stage);
                    }
                }
            },
        ));

        let model_name = effective_provider.model_name().to_string();

        // ── compact 配置 ──
        // 从 WorkflowAgentContext 读取 compact_config，与主 agent builder 模式一致。
        // 必须在 effective_provider 被 consume 之前构建 compact_llm。
        let compact_config = self.ctx.compact_config.clone();
        let context_budget = compact_config.as_ref().map(|cc| {
            ContextBudget::new(ContextBudget::DEFAULT_CONTEXT_WINDOW)
                .with_auto_compact_threshold(cc.auto_compact_threshold)
                .with_warning_threshold(cc.micro_compact_threshold)
        });
        // 本 run 的 retry observer：重试观测直接翻译为 LlmRetrying 交给本地 handler。
        let retry_observer =
            crate::session::retry_events::retry_observer_for(Arc::clone(&event_handler));

        let compact_llm: Option<Arc<dyn peri_model::Model>> = if compact_config.is_some() {
            Some(Arc::from(
                effective_provider
                    .clone()
                    .with_retry_observer(Some(retry_observer.clone()))
                    .into_model(),
            ))
        } else {
            None
        };

        // 前置条件（未文档化契约）：`ctx.agent_pool` 与主 builder 的 `ctx.pool` 必须是
        // 同一 `Arc<Mutex<AgentPool>>`——池化模型烘焙的 observer 才会与主链路共享同一
        // 转发器。当前 4 处入口均传 `agent_pool: None`（死路径），未来接线时须保证同源。
        let base_model: Arc<dyn peri_model::Model> = if let Some(ref pool) = self.ctx.agent_pool {
            let fp = format!(
                "{}:{}",
                effective_provider.display_name(),
                effective_provider.model_name()
            );
            AgentPool::get_or_create_subagent_llm(pool, &fp, || {
                // 池化分支：烘焙 session 级转发器的 observer（从 AgentPool 取值）。
                // 池化模型跨 run/跨 turn 存活，不能绑定 per-run 的本地 observer。
                effective_provider
                    .clone()
                    .with_retry_observer(Some(pool.lock().retry_events.as_retry_observer()))
                    .into_model()
            })
        } else {
            Arc::from(
                effective_provider
                    .with_retry_observer(Some(retry_observer))
                    .into_model(),
            )
        };

        // 2. 注册工具
        let mut tools: Vec<Box<dyn peri_agent::tools::BaseTool>> =
            FilesystemMiddleware::build_tools(&self.ctx.cwd);
        tools.extend(TerminalMiddleware::build_tools(&self.ctx.cwd));
        tools.extend(WebMiddleware::build_tools());
        // Workflow agent 无 plugin_skill_roots，仅 project-level skill 可用。
        // 在注册工具前扫描 project skills，预填充缓存（SkillTool 无懒扫描回退）。
        // D3：统一模型可见协议为 SkillTool(skill_name) + DiscoverSkillsTool，
        // 与主 agent / subagent 链一致，不再注册旧 Skill(skill, args)。
        let project_skills_root = std::path::PathBuf::from(&self.ctx.cwd)
            .join(".claude")
            .join("skills");
        let skills = peri_middlewares::skills::loader::scan_skill_roots(&[
            peri_middlewares::skills::SkillRoot {
                path: project_skills_root,
                source: peri_middlewares::skills::SkillSource::Project,
                plugin_name: None,
            },
        ]);
        let cached = std::sync::Arc::new(std::sync::RwLock::new(if skills.is_empty() {
            None
        } else {
            Some(skills)
        }));
        tools.push(Box::new(peri_middlewares::skills::tools::SkillTool::new(
            Arc::clone(&cached),
        )));
        tools.push(Box::new(
            peri_middlewares::skills::tools::DiscoverSkillsTool::new(cached),
        ));

        // 3. allowedTools 过滤
        if let Some(ref allowed) = params.allowed_tools {
            if !allowed.is_empty() {
                tools.retain(|t| allowed.contains(&t.name().to_string()));
            }
        }

        // 4. GAP-05: 使用标准 system prompt（复用 frozen 或运行时构建）
        let system_prompt = self.ctx.system_prompt.clone().unwrap_or_else(|| {
            // workflow agent 链不注册 WorkflowTool（无嵌套 workflow），
            // fallback 渲染关闭 workflow section，与工具注册一致。
            // detect_without_workflow：子 agent / fork / workflow agent 共用
            // 的 capability 快照（P2-2026-08-02）。
            let features = crate::prompt::PromptFeatures::detect_without_workflow(
                peri_middlewares::prelude::PermissionMode::Bypass,
            );
            let template = crate::prompt::PromptTemplate::new();
            let env = if let Some(ref date) = self.ctx.frozen_date {
                crate::prompt::PromptEnv::with_frozen_date(&self.ctx.cwd, date)
            } else {
                crate::prompt::PromptEnv::detect(&self.ctx.cwd)
            };
            template.render(&env, &features, &[], self.ctx.frozen_language.as_deref())
        });

        // 5. 构建中间件链
        let mut middlewares: Vec<Box<dyn peri_agent::middleware::r#trait::Middleware>> = Vec::new();

        let mut agents_md = AgentsMdMiddleware::new();
        if let Some(ref md) = self.ctx.frozen_claude_md {
            agents_md =
                agents_md.with_frozen_content(md.clone(), self.ctx.frozen_claude_local_md.clone());
        }
        middlewares.push(Box::new(agents_md));

        let mut skills_mw = SkillsMiddleware::new();
        if let Some(ref summary) = self.ctx.frozen_skill_summary {
            skills_mw = skills_mw.with_frozen_summary(summary.clone());
        }
        middlewares.push(Box::new(skills_mw));

        // GAP-01: SkillPreloadMiddleware（预加载 skill 全文，空列表 = 仅注册工具）
        middlewares.push(Box::new(SkillPreloadMiddleware::new(
            Vec::new(),
            &self.ctx.cwd,
        )));

        middlewares.push(Box::new(FilesystemMiddleware::new()));

        // 3a. GitAttributionMiddleware（在 FilesystemMiddleware 之后）
        middlewares.push(Box::new(GitAttributionMiddleware::new(&model_name)));

        middlewares.push(Box::new(TerminalMiddleware::new()));
        middlewares.push(Box::new(WebMiddleware::new()));

        // 3b. TodoMiddleware（在 WebMiddleware 之后）
        let (todo_tx, _todo_rx) = tokio::sync::mpsc::channel::<Vec<TodoItem>>(8);
        middlewares.push(Box::new(TodoMiddleware::new(todo_tx)));

        // GAP-03: HITL 审批中间件。
        // broker + permission_mode 均 Some 时启用审批（遵循 session 权限模式）；
        // 否则 Bypass（自主后台 agent 默认行为）。
        let hitl = match (&self.ctx.broker, &self.ctx.permission_mode) {
            (Some(broker), Some(mode)) => HumanInTheLoopMiddleware::with_shared_mode(
                Arc::clone(broker),
                default_requires_approval,
                Arc::clone(mode),
                None, // auto_classifier: workflow agent 不需要 LLM 分类器
            ),
            _ => HumanInTheLoopMiddleware::disabled(),
        };
        middlewares.push(Box::new(hitl));

        // [v2] CompactMiddleware 已移除——Workflow agent 的自动 compact 由 v2
        // stages/compact.rs 统一接管（run_react_loop 在每轮开头调 compact_v2::run_compact）。

        // 5. v2 stages 装配（替代 SubAgentBuilder）
        let cancel_token = self.ctx.cancel.clone().unwrap_or_default();
        let max_iterations = 200;

        // 组装 MiddlewareChain
        let mut chain = peri_agent::middleware::chain::MiddlewareChain::new();
        for mw in middlewares {
            chain.add(mw);
        }

        // tools: Vec<Box<dyn BaseTool>> → Vec<Arc<dyn BaseTool>>
        let tools_arc: Vec<Arc<dyn peri_agent::tools::BaseTool>> = tools
            .into_iter()
            .map(|t| Arc::from(t) as Arc<dyn peri_agent::tools::BaseTool>)
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
        let llm: Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync> = Box::new(base_llm);

        // error_suggest wiring（与 SubAgentBuilder.with_error_suggest() 等价）
        let all_tool_names: Vec<String> = tools_arc.iter().map(|t| t.name().to_string()).collect();
        let agents_dir = std::path::Path::new(&self.ctx.cwd)
            .join(".claude")
            .join("agents");
        let agents_dir_opt = if agents_dir.exists() {
            Some(agents_dir.as_path())
        } else {
            None
        };
        let snapshot = peri_middlewares::error_suggest::build_tool_registry_snapshot(
            all_tool_names,
            agents_dir_opt,
        );
        let error_suggest_registry = peri_middlewares::error_suggest::build_default_registry();

        // 构造 v2 StageContext（workflow agent 无 parent_messages）
        let v2_ctx = peri_middlewares::subagent::v2_bridge::build_v2_subagent_context(
            llm,
            chain,
            tools_arc,
            &self.ctx.cwd,
            cancel_token.clone(),
            Vec::new(),
            Some(system_prompt),
            None, // shared_tools
            compact_config,
            context_budget,
            compact_llm,
            Some(error_suggest_registry),
            Some(snapshot),
        );

        // EventBus forwarder（v2 → v1 ExecutorEvent，转发给 event_handler）
        // 用于 Langfuse trace + token usage 累积。
        //
        // 循环实现抽取至 `crate::event::forwarder::spawn_eventbus_forwarder`，
        // 以保证 biased select 顺序不变量与 main executor 调用点一致。
        let handler_for_forwarder = Arc::clone(&event_handler);
        crate::event::spawn_eventbus_forwarder(
            v2_ctx.event_handles,
            move |exec_ev| {
                handler_for_forwarder.on_event(exec_ev);
            },
            None, // bridge: workflow 在外部 handler 中处理 Langfuse
            None, // v2_tx
        );

        // push prompt 到 queue
        v2_ctx
            .context
            .session
            .queue
            .push(peri_agent::session::queue::QueuedMessage::new(
                peri_agent::session::queue::MessageKind::Prompt,
                peri_agent::session::queue::MessageSource::UserInput,
                peri_agent::messages::BaseMessage::human(params.prompt.clone()),
            ));

        // 7. 运行 v2 ReAct 循环
        let loop_result =
            peri_agent::agent::stages::run_react_loop(v2_ctx.context, max_iterations).await;

        let agent_result = match loop_result {
            peri_agent::agent::stages::LoopResult::Completed => {
                let output_text = extract_last_ai_text(&v2_ctx.session);

                // 获取 agent 执行期间累积的 token 用量
                let (total_output_tokens, last_model) = {
                    let s = usage_stats.lock();
                    let mut tokens = s.0;
                    // P0 fallback: haiku 等模型 usage=None 时 token 累积为 0，
                    // 按 output_text 长度启发式估算（每个 token ~4 字符）
                    if tokens == 0 && !output_text.is_empty() {
                        tokens = (output_text.len() as u64 / 4).max(1);
                    }
                    // P0 fallback: 如果事件从未 emit LlmCallEnd（如纯工具调用），
                    // 回退到 Node 传入的 model 参数
                    let model = s.1.clone().or_else(|| params.model.clone());
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
            peri_agent::agent::stages::LoopResult::Interrupted => {
                debug!("Workflow agent: execution interrupted");
                AgentRunResult::Dead {
                    reason: Some("interrupted".into()),
                    detail: Some("Workflow agent execution was interrupted".into()),
                }
            }
            peri_agent::agent::stages::LoopResult::Error(e) => {
                debug!(error = %e, "Workflow agent: execution failed");
                AgentRunResult::Dead {
                    reason: Some("runagent-threw".into()),
                    detail: Some(e.to_string()),
                }
            }
        };

        // GAP-08: 结束 Langfuse trace（fire-and-forget flush）
        if let Some(tracer) = langfuse_tracer {
            let error_output = match &agent_result {
                AgentRunResult::Dead { detail, .. } => detail.as_deref(),
                _ => None,
            };
            let handle = tracer.lock().on_turn_end(error_output);
            drop(handle); // fire-and-forget flush
        }

        agent_result
    }
}

/// 从 session transcript 提取最后一条非空 AI 消息文本
fn extract_last_ai_text(session: &Arc<peri_agent::session::Session>) -> String {
    let transcript = session.transcript();
    let tx = transcript.read();
    tx.visible_messages()
        .iter()
        .rev()
        .find_map(|m| {
            if matches!(m, peri_agent::messages::BaseMessage::Ai { .. }) {
                let t = m.content();
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            } else {
                None
            }
        })
        .unwrap_or_default()
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
