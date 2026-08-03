//! WorkflowMiddleware — connects workflow execution to the ReAct loop.
//!
//! 持有共享状态（runner / registry / progress_store / journal_store），
//! session 级创建，跨 turn 复用。
//!
//! `WorkflowMiddlewareAdaptor` 实现 `Middleware` trait，
//! 通过 `collect_tools()` 每轮提供 WorkflowTool 实例。
//! executor 在 `execute()` 开始时收集所有中间件工具并写入 `shared_tools`，
//! 然后 `ToolSearchMiddleware` 的 `before_agent` 从 `shared_tools` 构建搜索索引。

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use peri_agent::{
    agent::events::BackgroundTaskResult,
    error::AgentResult,
    middleware::{r#trait::Middleware, state::MiddlewareState},
    tools::BaseTool,
};
use peri_workflow::{
    journal::WorkflowJournalStore,
    progress::WorkflowProgressStore,
    registry::{WorkflowRun, WorkflowRunStatus, WorkflowTaskRegistry, WorkflowTaskResult},
    runner::{AgentExecutor, WorkflowInput, WorkflowResult, WorkflowRunner},
    tool::WorkflowTool,
};

use crate::subagent::{
    BackgroundTask, BackgroundTaskRegistry, BackgroundTaskStatus, BgCancelHandle, BgTaskKind,
};

/// 将 BackgroundTaskRegistry 适配为 peri-workflow 的 BgTaskRegistry trait
impl peri_workflow::tool::BgTaskRegistry for BackgroundTaskRegistry {
    fn register_workflow(&self, task_id: String, summary: String) {
        let bg_task = BackgroundTask {
            id: task_id,
            agent_name: "workflow".to_string(),
            prompt_summary: summary,
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            chrono_started_at: chrono::Utc::now(),
            kind: BgTaskKind::Workflow,
            cancel_handle: BgCancelHandle::Kill(None),
            pid: None,
            output_preview: None,
        };
        if let Err(e) = self.register_with_kind(bg_task) {
            tracing::warn!(error = %e, "workflow bg registry: register_with_kind failed");
        }
    }

    fn complete_workflow(&self, task_id: &str, success: bool, output: String, duration_ms: u64) {
        let result = BackgroundTaskResult {
            task_id: task_id.to_string(),
            agent_name: "workflow".to_string(),
            prompt_summary: String::new(),
            success,
            output: output.chars().take(500).collect(),
            tool_calls_count: 0,
            duration_ms,
            child_thread_id: None,
            timed_out: false,
        };
        self.complete(task_id, result);
    }
}

/// Workflow 中间件持有者——session 级共享状态，跨 turn 存活。
///
/// builder.rs 在 session/new 时创建，后续每轮 build_agent 复用。
/// 从中提取 WorkflowTool 注册为 deferred tool，
/// 同时保存 progress_store / registry 供外部（TUI 面板 / kill 命令）访问。
pub struct WorkflowMiddleware {
    runner: Arc<WorkflowRunner>,
    registry: Arc<WorkflowTaskRegistry>,
    progress_store: Arc<WorkflowProgressStore>,
    journal_store: Arc<WorkflowJournalStore>,
    /// 确保 notification consumer 只 spawn 一次（set-once gate）。
    /// 原 notification_buffer_rx 通道已迁移到 MessageQueue 模式，
    /// 保留此 gate 用于 session 级 consumer 去重。
    notification_consumer_spawned: AtomicBool,
    /// 统一后台任务注册表（可选，创建后可通过 set_bg_registry 延迟注入）
    bg_registry: parking_lot::RwLock<Option<Arc<BackgroundTaskRegistry>>>,
}

impl WorkflowMiddleware {
    /// 创建 WorkflowMiddleware（含完整共享状态）。
    ///
    /// `agent_executor`: workflow 内部 agent 回调执行器。
    /// `cwd`: 工作目录（runner / journal 共用）。
    /// `notification_tx`: workflow 完成通知通道（forwarder 转发到 ReAct 循环）。
    pub fn new(
        agent_executor: Arc<dyn AgentExecutor>,
        cwd: &str,
        notification_tx: tokio::sync::broadcast::Sender<
            peri_workflow::registry::WorkflowTaskResult,
        >,
        progress_rx: Option<
            tokio::sync::mpsc::UnboundedReceiver<peri_workflow::protocol::ProgressEvent>,
        >,
    ) -> Self {
        let runner = Arc::new(WorkflowRunner::new(agent_executor, cwd, progress_rx));
        let registry = Arc::new(WorkflowTaskRegistry::new(notification_tx));
        let progress_store = Arc::new(WorkflowProgressStore::new());
        let journal_store = Arc::new(WorkflowJournalStore::new(cwd));

        Self {
            runner,
            registry,
            progress_store,
            journal_store,
            notification_consumer_spawned: AtomicBool::new(false),
            bg_registry: parking_lot::RwLock::new(None),
        }
    }

    /// 设置统一后台任务注册表（构造时链式调用）
    pub fn with_bg_registry(self, bg_registry: Arc<BackgroundTaskRegistry>) -> Self {
        *self.bg_registry.write() = Some(bg_registry);
        self
    }

    /// 延迟注入 bg_registry（创建后设置，通过 RwLock 支持内部可变性）
    pub fn set_bg_registry(&self, bg_registry: Arc<BackgroundTaskRegistry>) {
        *self.bg_registry.write() = Some(bg_registry);
    }

    /// 创建一个新的 WorkflowTool 实例。
    pub fn create_tool(&self) -> WorkflowTool {
        let mut tool = WorkflowTool::new(
            Arc::clone(&self.runner),
            Arc::clone(&self.registry),
            Arc::clone(&self.progress_store),
            Arc::clone(&self.journal_store),
        );
        if let Some(ref bg) = *self.bg_registry.read() {
            tool = tool
                .with_bg_registry(Arc::clone(bg) as Arc<dyn peri_workflow::tool::BgTaskRegistry>);
        }
        tool
    }

    /// 获取 progress store（TUI 面板订阅用）。
    pub fn progress_store(&self) -> &Arc<WorkflowProgressStore> {
        &self.progress_store
    }

    /// 获取 registry（kill 命令用）。
    pub fn registry(&self) -> &Arc<WorkflowTaskRegistry> {
        &self.registry
    }

    /// 获取 runner（单 agent kill 用，GAP-07）。
    pub fn runner(&self) -> &Arc<WorkflowRunner> {
        &self.runner
    }

    /// 获取 journal store（resume 用）。
    pub fn journal_store(&self) -> &Arc<WorkflowJournalStore> {
        &self.journal_store
    }

    /// 恢复已完成的 workflow（GAP-04）。
    ///
    /// 读取旧运行的 state.json 获取脚本，以 `resume_from` 模式重新启动。
    /// 返回新的 run_id 字符串。
    ///
    /// # Errors
    ///
    /// 返回错误字符串：state 读取失败、workflow 仍在运行、注册失败等。
    pub async fn resume_workflow(&self, run_id: &str) -> Result<String, String> {
        let state = self
            .journal_store
            .read_state(run_id)
            .map_err(|e| format!("Failed to read workflow state: {e}"))?;

        if state.status == "running" || state.status == "active" {
            return Err("Workflow is still running, cannot resume".into());
        }

        let new_run_id = uuid::Uuid::now_v7().to_string();
        let wf_input = WorkflowInput {
            script: state.script.clone(),
            args: None,
            max_concurrency: 3,
            budget_total: None,
            workflow_name: state.workflow_name.clone(),
            resume_from: Some(run_id.to_string()),
        };
        let wf_name = wf_input.workflow_name.clone();

        let (done_tx, done_rx) = tokio::sync::watch::channel(None);
        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel();

        let runner = Arc::clone(&self.runner);
        let progress_store = Arc::clone(&self.progress_store);
        let journal_store = Arc::clone(&self.journal_store);
        let new_run_id_clone = new_run_id.clone();

        let started_at = std::time::Instant::now();
        let child_handle = tokio::spawn(async move {
            let _ = runner
                .run(
                    new_run_id_clone,
                    wf_input,
                    progress_store,
                    journal_store,
                    done_tx,
                    kill_rx,
                )
                .await;
        });

        let script_preview: String = state.script.chars().take(100).collect();
        self.registry
            .register(WorkflowRun {
                run_id: new_run_id.clone(),
                workflow_name: wf_name.clone(),
                script_preview,
                status: WorkflowRunStatus::Running,
                started_at,
                child_handle,
                kill_tx: Some(kill_tx),
            })
            .map_err(|e| format!("Failed to register resumed workflow: {e}"))?;

        // ─── 快速失败检测（1s 内 done 到来即同步报错）───
        let mut fast_rx = done_rx.clone();
        let fast_result = tokio::select! {
            _ = fast_rx.changed() => {
                let val = fast_rx.borrow().clone();
                if val.is_none() {
                    Some(WorkflowResult {
                        run_id: new_run_id.clone(),
                        status: "failed".to_string(),
                        return_value: None,
                        error: Some("workflow process exited before reporting result".to_string()),
                        stderr_tail: None,
                    })
                } else {
                    val
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => None,
        };

        if let Some(ref result) = fast_result {
            if result.status != "completed" {
                let error_msg = result
                    .error
                    .clone()
                    .unwrap_or_else(|| "workflow failed with no error details".to_string());
                let detail = result
                    .stderr_tail
                    .as_ref()
                    .map(|s| format!("\n\nstderr (last 20 lines):\n{}", s))
                    .unwrap_or_default();

                self.registry.complete(
                    &new_run_id,
                    WorkflowTaskResult {
                        run_id: new_run_id.clone(),
                        workflow_name: wf_name.clone(),
                        success: false,
                        status: WorkflowRunStatus::Failed,
                        duration_ms: started_at.elapsed().as_millis() as u64,
                        agent_count: 0,
                        tool_calls_count: 0,
                        error: Some(error_msg.clone()),
                        phase_summaries: Vec::new(),
                    },
                );

                return Err(format!(
                    "Workflow resume '{}' failed: {}{}",
                    wf_name, error_msg, detail
                ));
            }
        }
        // ─── 快速失败检测结束 ───

        // 完成后通知任务
        let registry_for_complete = Arc::clone(&self.registry);
        let notify_progress_store = Arc::clone(&self.progress_store);
        let notify_name = wf_name;
        let notify_run_id = new_run_id.clone();
        tokio::spawn(async move {
            let mut done_rx = done_rx;
            if done_rx.changed().await.is_err() {
                let (agent_count, tool_calls_count) = notify_progress_store
                    .get_run_stats(&notify_run_id)
                    .unwrap_or((0, 0));
                let phase_summaries = notify_progress_store.get_phase_summaries(&notify_run_id);
                registry_for_complete.complete(
                    &notify_run_id,
                    WorkflowTaskResult {
                        run_id: notify_run_id.clone(),
                        workflow_name: notify_name,
                        success: false,
                        status: WorkflowRunStatus::Failed,
                        duration_ms: started_at.elapsed().as_millis() as u64,
                        agent_count,
                        tool_calls_count,
                        error: Some("workflow process exited unexpectedly".to_string()),
                        phase_summaries,
                    },
                );
                return;
            }
            let result = done_rx
                .borrow()
                .clone()
                .expect("watch value should be Some after changed() resolves");
            let (agent_count, tool_calls_count) = notify_progress_store
                .get_run_stats(&notify_run_id)
                .unwrap_or((0, 0));
            let phase_summaries = notify_progress_store.get_phase_summaries(&notify_run_id);
            let success = result.status == "completed";
            let status = match result.status.as_str() {
                "completed" => WorkflowRunStatus::Completed,
                "killed" => WorkflowRunStatus::Killed,
                _ => WorkflowRunStatus::Failed,
            };
            registry_for_complete.complete(
                &notify_run_id,
                WorkflowTaskResult {
                    run_id: notify_run_id.clone(),
                    workflow_name: notify_name,
                    success,
                    status,
                    duration_ms: started_at.elapsed().as_millis() as u64,
                    agent_count,
                    tool_calls_count,
                    error: result.error,
                    phase_summaries,
                },
            );
        });

        Ok(new_run_id)
    }

    /// 订阅 workflow 完成通知。每轮 build_agent 调用一次，获取新的 Receiver。
    pub fn subscribe_notifications(
        &self,
    ) -> tokio::sync::broadcast::Receiver<peri_workflow::registry::WorkflowTaskResult> {
        self.registry.notification_tx().subscribe()
    }

    /// 首次调用返回 true（session 级 consumer spawn gate），后续返回 false。
    ///
    /// 原 notification_buffer channel 已迁移到 MessageQueue + broadcast 模式，
    /// 此方法仅保留 set-once 语义用于 executor 去重。
    pub fn init_notification_buffer(&self) -> bool {
        self.notification_consumer_spawned
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// Per-turn 中间件适配器——将 session 级 WorkflowMiddleware 接入中间件链。
///
/// builder.rs 每轮创建此适配器（持有 `Arc<WorkflowMiddleware>` + 当前轮 event_handler），
/// 通过 `collect_tools()` 让 executor 自动收集 WorkflowTool 到 `shared_tools`。
/// executor 在 `execute()` 开始时 clear + 重写 `shared_tools`，直接插入的工具会被清除，
/// 因此必须通过 `collect_tools()` 注册。
pub struct WorkflowMiddlewareAdaptor {
    inner: Arc<WorkflowMiddleware>,
}

impl WorkflowMiddlewareAdaptor {
    pub fn new(inner: Arc<WorkflowMiddleware>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Middleware for WorkflowMiddlewareAdaptor {
    fn name(&self) -> &str {
        "WorkflowMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(self.inner.create_tool())]
    }

    async fn before_agent(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_workflow::protocol::{AgentRunParams, AgentRunResult, Usage};

    struct MockAgentExecutor;

    #[async_trait]
    impl AgentExecutor for MockAgentExecutor {
        async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
            AgentRunResult::Ok {
                output: "mock".into(),
                usage: Usage { output_tokens: 0 },
                model: None,
                tool_count: None,
                token_count: None,
                phase: None,
                duration_ms: None,
            }
        }
    }

    fn make_middleware() -> Arc<WorkflowMiddleware> {
        let executor: Arc<dyn AgentExecutor> = Arc::new(MockAgentExecutor);
        let (notification_tx, _) = tokio::sync::broadcast::channel(32);
        Arc::new(WorkflowMiddleware::new(
            executor,
            "/tmp",
            notification_tx,
            None,
        ))
    }

    /// [回归测试] WorkflowTool 注册面与 prompt gate 共用同一条件源（阶段 3）。
    ///
    /// 历史背景（审计 prompt-sections-audit.md P1-5）：16_workflow 原无条件
    /// 渲染，而 WorkflowTool 注册严格依赖 `workflow_executor.is_some()`。
    /// 此测试锁定注册面：Adaptor 装配后 collect_tools 必须产出 WorkflowTool
    ///（deferred）；prompt 面由 peri-acp prompt_test 的 Workflow gate 覆盖，
    /// 搜索面由 tool_search/middleware_test 的用例覆盖。
    #[test]
    fn test_adaptor_collect_tools_returns_workflow_tool() {
        let mw = make_middleware();
        let adaptor = WorkflowMiddlewareAdaptor::new(mw);
        let tools = adaptor.collect_tools("/tmp");
        assert_eq!(tools.len(), 1, "Adaptor 恰好提供 WorkflowTool");
        assert_eq!(tools[0].name(), "Workflow");
        assert!(
            !tools[0].is_direct(),
            "WorkflowTool 是 deferred tool，不得直接进入 LLM tools"
        );
    }
}
