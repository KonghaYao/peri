//! 共享后台 spawn 逻辑
//!
//! `spawn_background_fork()` 提取自 `SubAgentTool::invoke_background_fork`，
//! 供 ACP 层（/bg 斜杠命令）和工具路径共同使用。
//!
//! 调用者负责提供所有需要的依赖，包括 LLM 实例、工具集、线程存储等。

use std::path::PathBuf;
use std::sync::Arc;

use peri_agent::{
    agent::{
        events::AgentEvent, react::AgentInput, state::AgentState, AgentCancellationToken,
        BackgroundTaskResult, ReActAgent, State as _,
    },
    messages::BaseMessage,
    thread::ThreadMeta,
};

use super::tool::{build_subagent_middlewares, fire_subagent_lifecycle_hooks_static};
use crate::{
    hooks::types::{HookEvent, RegisteredHook},
    subagent::{
        background::{BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus},
        SubAgentMiddlewareConfig,
    },
    tools::ArcToolWrapper,
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
    /// 派发给子 Agent 的任务描述
    pub prompt: String,
    /// 父会话的消息历史（用于子 Agent 理解上下文）
    pub parent_messages: Vec<BaseMessage>,
    /// 工作目录
    pub cwd: PathBuf,
    /// LLM 实例（ReactLLM trait object，spawner 内部包装 RetryableLLM）
    pub llm: Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync>,
    /// 最大 ReAct 迭代次数
    pub max_iterations: usize,
    /// 父 Agent 的工具集（子 Agent 继承）
    pub parent_tools: Arc<Vec<Arc<dyn peri_agent::tools::BaseTool>>>,
    /// 已注册的 hooks（用于 SubagentStart/SubagentStop 生命周期事件）
    pub registered_hooks: Arc<Vec<RegisteredHook>>,
    /// 线程持久化存储（可选）
    pub thread_store: Option<Arc<dyn peri_agent::thread::ThreadStore>>,
    /// 父线程 ID（用于子线程层级关系）
    pub parent_thread_id: Option<String>,
    /// 运行时注册回调：(thread_id, cancel_token, cancel_policy_str)
    #[allow(clippy::type_complexity)]
    pub register_runtime: Option<Arc<dyn Fn(String, AgentCancellationToken, String) + Send + Sync>>,
    /// 运行时注销回调：&thread_id
    pub deregister_runtime: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// 后台任务完成事件的发送通道（必填）
    pub bg_event_sender: tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::AgentEvent>,
    /// 后台任务注册中心
    pub bg_registry: Arc<BackgroundTaskRegistry>,
    /// Fork 指令类型：BGFork 使用中文 bg-fork directive，普通使用英文 fork directive
    pub fork_directive_kind: BgForkDirectiveKind,
}

/// 后台 fork agent spawn 结果
pub struct BgForkSpawned {
    /// 后台任务 ID（格式：bg-{uuid v7}）
    pub task_id: String,
    /// 子线程 ID（uuid v7）
    pub child_thread_id: String,
}

/// 启动后台 fork agent
///
/// 复制 `SubAgentTool::invoke_background_fork` 的完整行为：
/// 1. 并发检查（最多 3 个活跃任务）
/// 2. 生成 task_id 和 child_thread_id
/// 3. 创建子线程（通过 thread_store）
/// 4. 构建 fork directive（根据 fork_directive_kind 选择模板）
/// 5. 构建 ReActAgent + RetryableLLM + subagent middlewares
/// 6. 注册 parent_tools
/// 7. 设置 BgToolStep 事件转发
/// 8. 创建 AgentState，注入 parent_messages + directive
/// 9. tokio::spawn 执行
/// 10. 注册到 BackgroundTaskRegistry（oneshot 信号量保证时序）
/// 11. 返回 BgForkSpawned
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
    let has_thread_store = config.thread_store.is_some();
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
        BgForkDirectiveKind::Bg => super::fork::build_bg_fork_directive(&config.prompt),
        BgForkDirectiveKind::Fork => super::fork::build_fork_directive(&config.prompt),
    };

    // 5. 构建 ReActAgent（包装 RetryableLLM）
    let llm =
        peri_agent::llm::RetryableLLM::new(config.llm, peri_agent::llm::RetryConfig::default());
    let mut agent_builder = ReActAgent::new(llm).max_iterations(config.max_iterations);
    for mw in build_subagent_middlewares(SubAgentMiddlewareConfig::for_fork(&cwd)) {
        agent_builder = agent_builder.add_middleware(mw);
    }

    // 6. 注册父工具集
    for tool in config.parent_tools.iter() {
        agent_builder = agent_builder.register_tool(Box::new(ArcToolWrapper(Arc::clone(tool))));
    }

    // 7. 克隆 spawn 所需数据
    let spawn_registry = Arc::clone(&config.bg_registry);
    let spawn_hooks = Arc::clone(&config.registered_hooks);
    let spawn_bg_sender = config.bg_event_sender.clone();
    let spawn_task_id = task_id.clone();
    let spawn_agent_name = agent_name.clone();
    let spawn_prompt_summary = prompt_summary.clone();
    let spawn_thread_store = config.thread_store.clone();
    let spawn_child_thread_id = child_thread_id.clone();
    let spawn_deregister_runtime = config.deregister_runtime.clone();
    let spawn_parent_messages = config.parent_messages.clone();
    let spawn_has_thread_store = has_thread_store;

    // 8. 注册 AgentRuntime
    // Independent: child_cancel 不与父 cancel 关联，仅 session 级 cancel_all_agents 可取消
    let child_cancel = if spawn_has_thread_store {
        if let Some(ref register) = config.register_runtime {
            let cc = AgentCancellationToken::new();
            register(
                child_thread_id.clone(),
                cc.clone(),
                "independent".to_string(),
            );
            Some(cc)
        } else {
            None
        }
    } else {
        None
    };
    let cancel_token = child_cancel;

    // 9. 触发 SubagentStart hook
    fire_subagent_lifecycle_hooks_static(
        &config.registered_hooks,
        HookEvent::SubagentStart,
        &cwd,
        &agent_name,
        None,
    )
    .await;

    // 10. 设置 BgToolStep 事件转发（用于 TUI bg_agent_bar 实时计数）
    let step_sender = config.bg_event_sender.clone();
    let step_ctid = child_thread_id.clone();
    agent_builder = agent_builder.with_event_handler(Arc::new(
        peri_agent::agent::events::FnEventHandler(move |event: AgentEvent| {
            if matches!(event, AgentEvent::ToolStart { .. }) {
                let _ = step_sender.send(AgentEvent::BgToolStep {
                    child_thread_id: step_ctid.clone(),
                });
            }
        }),
    ));

    // 11. 注册到 BackgroundTaskRegistry（必须在 spawn 之前，避免竞态窗口）
    let (start_tx, start_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        // 等待注册完成信号后才开始执行
        let _ = start_rx.await;

        let mut fork_state = if let Some(ref store) = spawn_thread_store {
            AgentState::new(&cwd).with_persistence(Arc::clone(store), spawn_child_thread_id.clone())
        } else {
            AgentState::new(&cwd)
        };
        // 注入父消息历史
        for msg in spawn_parent_messages {
            fork_state.add_message(msg);
        }
        let start = std::time::Instant::now();

        let result = match agent_builder
            .execute(
                AgentInput::text(&fork_directive),
                &mut fork_state,
                cancel_token,
            )
            .await
        {
            Ok(output) => {
                let tool_calls_count = fork_state
                    .messages
                    .iter()
                    .filter(|m| matches!(m, BaseMessage::Tool { .. }))
                    .count();
                BackgroundTaskResult {
                    task_id: spawn_task_id.clone(),
                    agent_name: spawn_agent_name.clone(),
                    prompt_summary: spawn_prompt_summary.clone(),
                    success: true,
                    output: output.text,
                    tool_calls_count,
                    duration_ms: start.elapsed().as_millis() as u64,
                    child_thread_id: Some(spawn_child_thread_id.clone()),
                }
            }
            Err(e) => BackgroundTaskResult {
                task_id: spawn_task_id.clone(),
                agent_name: spawn_agent_name.clone(),
                prompt_summary: spawn_prompt_summary.clone(),
                success: false,
                output: e.to_string(),
                tool_calls_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                child_thread_id: Some(spawn_child_thread_id.clone()),
            },
        };

        // 更新子线程状态
        if let Some(ref store) = spawn_thread_store {
            let status = if result.success { "done" } else { "error" };
            let _ = store
                .update_thread_status(&spawn_child_thread_id, status)
                .await;
        }

        spawn_registry.complete(&spawn_task_id, result.clone());

        fire_subagent_lifecycle_hooks_static(
            &spawn_hooks,
            HookEvent::SubagentStop,
            &cwd,
            &spawn_agent_name,
            Some(&result.output),
        )
        .await;

        // 通过独立通道发送完成事件
        tracing::info!(
            task_id = %spawn_task_id,
            agent_name = %spawn_agent_name,
            success = result.success,
            "[bg-diag] bg-task sending BackgroundTaskCompleted via bg_event_tx"
        );
        let _ = spawn_bg_sender.send(AgentEvent::BackgroundTaskCompleted(result));

        // 注销 AgentRuntime
        if let Some(ref deregister) = spawn_deregister_runtime {
            if spawn_has_thread_store {
                deregister(&spawn_child_thread_id);
            }
        }
    });

    // 注册到 BackgroundTaskRegistry（JoinHandle 已就绪）
    config.bg_registry.register(BackgroundTask {
        id: task_id.clone(),
        agent_name: agent_name.clone(),
        prompt_summary,
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        abort_handle: handle,
    })?;

    // 发送启动信号，解除 task 内的阻塞
    let _ = start_tx.send(());

    // 通知 TUI 后台任务启动（用于状态栏 bg 列表显示）
    let _ = config.bg_event_sender.send(AgentEvent::SubagentStarted {
        agent_name: agent_name.clone(),
        instance_id: child_thread_id.clone(),
        is_background: true,
    });

    tracing::info!(
        task_id = %task_id,
        child_thread_id = %child_thread_id,
        agent_name = %agent_name,
        "[bg-diag] background agent started via spawner"
    );

    Ok(BgForkSpawned {
        task_id,
        child_thread_id,
    })
}
