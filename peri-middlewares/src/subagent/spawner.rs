//! 共享后台 spawn 逻辑
//!
//! `spawn_background_fork()` 提取自 `SubAgentTool::invoke_background_fork`，
//! 供 ACP 层（/bg 斜杠命令）和工具路径共同使用。
//!
//! **P5.1 重构**：从 v1 ReactLLM 实现改为 v2 stages，
//! 内部通过 `build_v2_subagent_context` 构造 StageContext，`tokio::spawn`
//! 内运行 `run_react_loop`。

use std::path::PathBuf;
use std::sync::Arc;

use peri_agent::agent::LangfuseBridgeLike;
use peri_agent::{
    agent::{
        events::ExecutorEvent,
        stages::{run_react_loop, LoopResult},
    },
    messages::BaseMessage,
    middleware::chain::MiddlewareChain,
    thread::ThreadMeta,
    tools::BaseTool,
};
use tokio_util::sync::CancellationToken;

use crate::{
    hooks::types::{HookEvent, RegisteredHook},
    subagent::{
        background::{
            BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus, BgCancelHandle,
            BgTaskKind,
        },
        tool::lifecycle::emit_subagent_stop_bg,
        v2_bridge::build_v2_subagent_context,
        SubAgentMiddlewareConfig,
    },
};

/// Fork 指令类型，决定 fork agent 使用的 system directive 模板
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgForkDirectiveKind {
    /// 使用 build_fork_directive()（英文，Agent 工具路径）
    Fork,
    /// 使用 build_bg_fork_directive()（中文，/bg 命令路径）
    Bg,
}

/// 后台 fork agent 启动配置
///
/// 所有字段为 spawn_background_fork 的必要依赖，
/// 从 SubAgentMiddleware 或 ACP 层的对应字段映射而来。
pub struct BgForkConfig {
    /// 派发给子 Agent 的任务描述（不含 fork directive 包装）
    pub prompt: String,
    /// 父会话的消息历史（用于子 Agent 理解上下文）
    pub parent_messages: Vec<BaseMessage>,
    /// 工作目录
    pub cwd: PathBuf,
    /// LLM 实例（ReactLLM trait object）
    pub llm: Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync>,
    /// 最大 ReAct 迭代次数
    pub max_iterations: usize,
    /// 父 Agent 的工具集（子 Agent 继承）
    pub parent_tools: Arc<Vec<Arc<dyn BaseTool>>>,
    /// 已注册的 hooks（用于 SubagentStart/SubagentStop 生命周期事件）
    pub registered_hooks: Arc<Vec<RegisteredHook>>,
    /// 线程持久化存储（可选）
    pub thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    /// 父线程 ID（用于子线程层级关系）
    pub parent_thread_id: Option<String>,
    /// 运行时注册回调：(thread_id, cancel_token, cancel_policy_str)
    #[allow(clippy::type_complexity)]
    pub register_runtime: Option<Arc<dyn Fn(String, CancellationToken, String) + Send + Sync>>,
    /// 运行时注销回调：&thread_id
    pub deregister_runtime: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// 后台任务完成事件的发送通道（必填）
    pub bg_event_sender: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    /// 后台任务注册中心
    pub bg_registry: Arc<BackgroundTaskRegistry>,
    /// Fork 指令类型：BGFork 使用中文 bg-fork directive，普通使用英文 fork directive
    pub fork_directive_kind: BgForkDirectiveKind,
    /// bg 完成时的同步回调：在 registry.complete() 之前调用
    /// 用于将 bg 结果（Defer 消息）同步推入主 agent 的 MQ
    pub on_bg_complete:
        Option<Arc<dyn Fn(&peri_agent::agent::events::BackgroundTaskResult) + Send + Sync>>,
    /// Frozen CLAUDE.md main content（session/new 时捕获，SubAgent 复用以避免漂移）
    pub frozen_claude_md: Option<Arc<String>>,
    /// Frozen CLAUDE.local.md content
    pub frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen skills summary
    pub frozen_skill_summary: Option<Arc<String>>,
    /// Frozen system prompt（fork 路径复用以避免重建）。
    pub frozen_system_prompt: Option<Arc<String>>,
    /// Langfuse bridge for subagent trace（None 表示遥测禁用）
    pub langfuse_bridge: Option<Arc<dyn LangfuseBridgeLike>>,
}

/// 后台 fork agent spawn 结果
pub struct BgForkSpawned {
    /// 后台任务 ID（格式：bg-{uuid v7}）
    pub task_id: String,
    /// 子线程 ID（uuid v7）
    pub child_thread_id: String,
    /// SubagentStarted 事件（构造好但**未发送**，由调用方决定推送路径）。
    ///
    /// - `/bg` 命令（BgCommand）：通过 `event_sink.push_event` 同步推送，保证
    ///   TUI 在 Done 之前收到（避免 race condition）
    /// - Agent 工具路径（SubAgentTool）：通过 `bg_event_sender` 异步推送
    ///   （主 agent 在跑 `loading=true`，无 race）
    pub started_event: ExecutorEvent,
}

/// 启动后台 fork agent（v2 stages）
///
/// 1. 并发检查（最多 3 个活跃任务）
/// 2. 生成 task_id 和 child_thread_id
/// 3. 创建子线程（cancel_policy=independent）
/// 4. 构建 fork directive
/// 5. 组装 v2 middlewares + chain
/// 6. 构造 StageContext（注入 parent_messages 到 transcript）
/// 7. push directive 到 queue
/// 8. tokio::spawn 运行 `run_react_loop`
/// 9. 注册到 BackgroundTaskRegistry
/// 10. 返回 BgForkSpawned
pub async fn spawn_background_fork(
    config: BgForkConfig,
) -> Result<BgForkSpawned, Box<dyn std::error::Error + Send + Sync>> {
    // 1. 并发检查
    if config.bg_registry.active_count() >= 3 {
        return Err("已有 3 个后台任务在运行".into());
    }

    // 2. 生成标识符
    let task_id = format!("bg-{}", uuid::Uuid::now_v7());
    let child_thread_id = uuid::Uuid::now_v7().to_string();
    let agent_name = "fork".to_string();
    let prompt_summary: String = config.prompt.chars().take(100).collect();
    let cwd = config.cwd.to_string_lossy().to_string();

    // 3. 创建子线程
    if let Some(ref store) = config.thread_store {
        let snapshot_id = config
            .parent_messages
            .last()
            .map(|m| m.id().as_uuid().to_string());
        let mut child_meta = ThreadMeta::new(&cwd);
        child_meta.id = child_thread_id.clone();
        child_meta.parent_thread_id = config.parent_thread_id.clone();
        child_meta.snapshot_at_message_id = snapshot_id;
        child_meta.hidden = true;
        child_meta.cancel_policy = "independent".parse().expect("合法 cancel_policy 字符串");
        child_meta.title = Some(format!("bg-fork-{}", task_id));
        store
            .create_thread(child_meta)
            .await
            .map_err(|e| format!("Failed to create child thread: {}", e))?;
    }

    // 4. 根据 directive_kind 选择指令模板
    let fork_directive = match config.fork_directive_kind {
        BgForkDirectiveKind::Bg => crate::subagent::fork::build_bg_fork_directive(&config.prompt),
        BgForkDirectiveKind::Fork => crate::subagent::fork::build_fork_directive(&config.prompt),
    };

    // 5. 组装 v2 middlewares + chain
    let mw_config = SubAgentMiddlewareConfig::for_agent_def(Vec::new(), &cwd).with_frozen(
        config
            .frozen_claude_md
            .as_deref()
            .map(|s| s.as_str().to_string()),
        config
            .frozen_claude_local_md
            .as_deref()
            .map(|s| s.as_str().to_string()),
        config
            .frozen_skill_summary
            .as_deref()
            .map(|s| s.as_str().to_string()),
    );
    let middlewares = crate::subagent::tool::build_subagent_middlewares(mw_config);
    let mut chain = MiddlewareChain::new();
    for mw in middlewares {
        chain.add(mw);
    }

    // 6. tools：parent_tools 已是 Arc<Vec<Arc<dyn BaseTool>>>
    let tools: Vec<Arc<dyn BaseTool>> = config.parent_tools.iter().cloned().collect();

    // 7. Independent cancel token
    let cancel_token = CancellationToken::new();

    // 8. 构造 v2 StageContext（注入 parent_messages 到 transcript）
    let v2_ctx = build_v2_subagent_context(
        config.llm,
        chain,
        tools,
        &cwd,
        cancel_token.clone(),
        config.parent_messages,
        config
            .frozen_system_prompt
            .clone()
            .map(|sp| sp.as_ref().to_string()),
        None,
        None,
        None,
        None,
        None,
        None,
    );

    // 9. push fork_directive 到 queue
    v2_ctx
        .context
        .session
        .queue
        .push(peri_agent::session::queue::QueuedMessage::new(
            peri_agent::session::queue::MessageKind::Prompt,
            peri_agent::session::queue::MessageSource::UserInput,
            BaseMessage::human(fork_directive),
        ));

    // 10. 注册到 active_agents
    if let Some(register) = &config.register_runtime {
        register(child_thread_id.clone(), cancel_token, "independent".into());
    }

    // 构造 SubagentStarted 事件（不发送——由调用方决定推送路径）。
    // 见 BgForkSpawned::started_event 字段注释。
    let started_event = ExecutorEvent::SubagentStarted {
        agent_name: agent_name.clone(),
        instance_id: child_thread_id.clone(),
        is_background: true,
    };

    // 11. 捕获 spawn 资源
    let on_bg_complete = config.on_bg_complete.clone();
    let thread_store = config.thread_store.clone();
    let deregister_runtime = config.deregister_runtime.clone();
    let bg_event_sender = config.bg_event_sender;
    let bg_registry = Arc::clone(&config.bg_registry);
    let registered_hooks = Arc::clone(&config.registered_hooks);
    let task_id_clone = task_id.clone();
    let child_thread_id_clone = child_thread_id.clone();
    let agent_name_clone = agent_name.clone();
    let prompt_summary_clone = prompt_summary.clone();
    let cwd_clone = cwd.clone();
    let max_iterations = config.max_iterations;
    let event_handles = v2_ctx.event_handles;
    let langfuse_bridge = config.langfuse_bridge;

    // 12. tokio::spawn 执行
    let join_handle = tokio::spawn(async move {
        let started_at = std::time::Instant::now();
        let context = v2_ctx.context;
        let session = v2_ctx.session;

        // 启动 v2 事件转发器：消费 SubAgent EventBus 的事件，注入 source_agent_id
        // 后转发到 bg_event_sender。同时桥接 Langfuse trace。
        let bg_sender_for_forwarder = bg_event_sender.clone();
        let bg_forwarder_handler: Option<Arc<dyn peri_agent::agent::events::AgentEventHandler>> =
            Some(Arc::new(peri_agent::agent::events::FnEventHandler(
                move |ev: ExecutorEvent| {
                    let _ = bg_sender_for_forwarder.send(ev);
                },
            ))
                as Arc<dyn peri_agent::agent::events::AgentEventHandler>);
        let _forwarder_handle =
            peri_agent::agent::subagent_event_forwarder::spawn_subagent_event_forwarder(
                event_handles,
                bg_forwarder_handler,
                langfuse_bridge,
                child_thread_id_clone.clone(),
            );

        let loop_result = run_react_loop(context, max_iterations).await;

        let (final_text, interrupted) = match loop_result {
            LoopResult::Completed => (extract_last_ai_text(&session), false),
            LoopResult::Interrupted => (String::new(), true),
            LoopResult::Error(e) => {
                let output = format!("Background fork agent failed: {}", e);
                // 错误路径：lifecycle hook + thread_store + registry notification
                fire_stop_hooks(&registered_hooks, &cwd_clone, &agent_name_clone, &output).await;
                if let Some(ref store) = thread_store {
                    let _ = store
                        .update_thread_status(&child_thread_id_clone, "error")
                        .await;
                }
                // 错误分支也必须发射 SubagentStopped（is_error=true），保证 depth 配对减 1。
                // 必须在 BackgroundTaskResult 构造之前发射——后者会 move output。
                emit_subagent_stop_bg(
                    &bg_event_sender,
                    &agent_name_clone,
                    output.clone(),
                    true,
                    &child_thread_id_clone,
                );
                let result = peri_agent::agent::events::BackgroundTaskResult {
                    task_id: task_id_clone.clone(),
                    agent_name: agent_name_clone.clone(),
                    prompt_summary: prompt_summary_clone.clone(),
                    success: false,
                    output,
                    tool_calls_count: crate::subagent::count_tool_calls_from_session(&session),
                    duration_ms: started_at.elapsed().as_millis() as u64,
                    child_thread_id: Some(child_thread_id_clone.clone()),
                    timed_out: false,
                };
                // 同步推送 Defer 到 MQ——必须在 registry.complete() 之前
                if let Some(ref on_complete) = on_bg_complete {
                    on_complete(&result);
                }
                bg_registry.complete(&task_id_clone, result);
                if let Some(deregister) = &deregister_runtime {
                    deregister(&child_thread_id_clone);
                }
                return;
            }
        };

        let output_summary: String = if interrupted {
            "interrupted".to_string()
        } else {
            final_text.chars().take(500).collect()
        };

        // SubagentStop lifecycle hook
        fire_stop_hooks(
            &registered_hooks,
            &cwd_clone,
            &agent_name_clone,
            &output_summary,
        )
        .await;

        // thread_store 状态
        if let Some(ref store) = thread_store {
            let status = if interrupted { "cancelled" } else { "done" };
            let _ = store
                .update_thread_status(&child_thread_id_clone, status)
                .await;
        }

        // 后台任务完成通知
        let result = peri_agent::agent::events::BackgroundTaskResult {
            task_id: task_id_clone.clone(),
            agent_name: agent_name_clone.clone(),
            prompt_summary: prompt_summary_clone.clone(),
            success: !interrupted,
            output: if interrupted {
                "Background fork agent was interrupted".to_string()
            } else {
                final_text
            },
            tool_calls_count: crate::subagent::count_tool_calls_from_session(&session),
            duration_ms: started_at.elapsed().as_millis() as u64,
            child_thread_id: Some(child_thread_id_clone.clone()),
            timed_out: false,
        };
        // 同步推送 Defer 到 MQ——必须在 registry.complete() 之前
        // 确保 active_count 归零时 Defer 已在 MQ 中
        if let Some(ref on_complete) = on_bg_complete {
            on_complete(&result);
        }
        // 先发射 SubagentStopped（与 SubagentStarted 配对），让 TUI 把 subagent_depth
        // 减 1（mod.rs SubAgentEnd 处理），避免 depth 永久累积导致 token tracker 失效。
        emit_subagent_stop_bg(
            &bg_event_sender,
            &agent_name_clone,
            output_summary.clone(),
            interrupted,
            &child_thread_id_clone,
        );
        if let Err(e) = bg_event_sender.send(ExecutorEvent::BackgroundTaskCompleted(result.clone()))
        {
            tracing::warn!(error = ?e, "bg fork: failed to send completion event");
        }
        bg_registry.complete(&task_id_clone, result);

        if let Some(deregister) = &deregister_runtime {
            deregister(&child_thread_id_clone);
        }
    });

    // 13. 注册到 BackgroundTaskRegistry
    let bg_task = BackgroundTask {
        id: task_id.clone(),
        agent_name: agent_name.clone(),
        prompt_summary,
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(join_handle.abort_handle()),
        pid: None,
        output_preview: None,
    };
    if let Err(e) = config.bg_registry.register_with_kind(bg_task) {
        return Err(format!("Failed to register background fork task: {}", e).into());
    }

    Ok(BgForkSpawned {
        task_id,
        child_thread_id,
        started_event,
    })
}

/// 触发 SubagentStop 生命周期 hook
async fn fire_stop_hooks(
    registered_hooks: &Arc<Vec<RegisteredHook>>,
    cwd: &str,
    agent_name: &str,
    result: &str,
) {
    crate::subagent::tool::fire_subagent_lifecycle_hooks_static(
        registered_hooks,
        HookEvent::SubagentStop,
        cwd,
        agent_name,
        Some(result),
    )
    .await;
}

/// 从 session transcript 提取最后一条非空 AI 消息文本
fn extract_last_ai_text(session: &Arc<peri_agent::session::Session>) -> String {
    // P1-11: 委托给 tool::extract_last_ai_text（tool/mod.rs 共用实现）
    super::tool::extract_last_ai_text(session)
}
