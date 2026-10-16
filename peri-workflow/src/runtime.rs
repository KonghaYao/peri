//! WorkflowRuntime —— workflow 编排的领域控制接口（MCP adapter 背后的唯一控制面）。
//!
//! 设计依据：`docs/design/workflow-mcp.md` §3。MCP 对外契约（工具 / 资源 / 订阅）
//! 经 `peri-mcp-workflow` 的 handler 表达；Node wire（`workflow/start`、`agent/task`
//! 等）是本类型的实现细节。旧 middleware 链在迁移期经本类型复用同一份状态。

mod close_gate;
pub mod event_log;
mod events;
pub mod params;
mod tasks;

pub use close_gate::{AdmissionTicket, CloseGate};
pub use event_log::{BoundedEventLog, EventLogPage};
pub use events::{WorkflowRunEvent, WorkflowRunEventKind, WorkflowRunEventLog};
pub use tasks::{
    ClaimOutcome, PublishOutcome, SettleAdmission, SettleOutcome, TaskBaseline, WorkflowTaskEvent,
    WorkflowTaskEventKind, WorkflowTaskRecord, WorkflowTaskState, WorkflowTaskTable,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedReceiver;

use peri_acp_types::tasks::TaskManager;

use crate::journal::WorkflowJournalStore;
use crate::progress::{RunProgress, WorkflowProgressStore};
use crate::protocol::{AgentRunParams, AgentRunResult, ProgressEvent};
use crate::registry::{WorkflowTaskRegistry, WorkflowTaskResult};
use crate::runner::{
    AgentExecutor, AgentTaskPublish, AgentTaskSink, WorkflowInput, WorkflowRunner,
};

use events::RuntimeProgressObserver;

/// run 事件日志容量（保留边界随淘汰前进）。
const RUN_EVENT_LOG_CAPACITY: usize = 1024;
/// 订阅广播容量（唤醒信号；滞后接收方经 read 补读，不做回压）。
const UPDATE_CHANNEL_CAPACITY: usize = 64;
/// run 预算条目的启动保护窗口：窗口内即使 progress 快照尚未出现也不回收。
const BUDGET_GRACE: Duration = Duration::from_secs(60);

/// 消费者绑定令牌的调用元数据键（`WorkflowAgentResult` 回填的调用授权，design §4.2）。
///
/// 令牌由宿主消费者经 [`WorkflowRuntime::bind_consumer_token`] 取得（进程内共享），
/// 随宿主调用的 `_meta` 透传；模型面拿不到该值。
pub const WORKFLOW_CONSUMER_META_KEY: &str = "peri/workflowConsumer";

/// 资源更新信号：订阅唤醒用，正文一律经 `resources/read` 补读（design §4.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowResourceUpdate {
    /// `workflow://runs` 索引变化。
    RunsIndex,
    /// 单 run 快照变化（`workflow://runs/{runId}`）。
    Run { run_id: String },
    /// run 事件日志追加（`workflow://runs/{runId}/events`）。
    RunEvents { run_id: String },
    /// run 任务事件追加（`workflow://runs/{runId}/tasks`）。
    RunTasks { run_id: String },
    /// session 级任务事件追加（`workflow://tasks`）。
    Tasks,
}

/// 启动结果：run_id + 名称（工具 / ACP / 日志共用同一文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowStartOutcome {
    pub run_id: String,
    pub workflow_name: String,
}

impl WorkflowStartOutcome {
    /// 启动成功文案（稳定格式；MCP `Workflow` 工具与旧 `WorkflowTool` 共用）。
    ///
    /// 与完成提醒同口径说明长文本外置（`${label}` 是引用、内容在 `outputs/`）：
    /// 启动期先说清，完成提醒才不会被读成「脚本未求值」。
    pub fn message(&self) -> String {
        format!(
            "Workflow '{workflow_name}' started.\n\
             run_id: {run_id}\n\
             \n\
             The workflow is running in the background.\n\
             You will be notified when it completes with a result summary.\n\
             Results will be saved to .claude/workflow-runs/{run_id}/state.json\n\
             Long string fields in the result object (over {threshold} bytes) are externalized: a \
             `${{label}}` token in state.json is that field's path, and its text lives in \
             `outputs/` in that directory under the same label with a `.txt` suffix.",
            workflow_name = self.workflow_name,
            run_id = self.run_id,
            threshold = peri_acp_types::workflow::LONGTEXT_EXTRACT_THRESHOLD_BYTES,
        )
    }
}

/// 单 run 的执行权威预算（design §5.2 第 5 条：maxConcurrency / limits 同一权威）。
///
/// 宿主消费者与 wire 准入（`agent/task`）都从这里取值，**不另立计数**：容量判定
/// 只叠加任务表的执行中记录（任务表是运行期状态的单一真相）。
#[derive(Debug, Clone, Copy)]
pub struct RunExecutionBudget {
    /// run 并发上界（`maxConcurrency`）：达到上限的任务留在 `published` 排队。
    pub max_concurrency: u32,
    pub max_agents: Option<u64>,
    pub max_tool_calls: Option<u64>,
}

struct BudgetEntry {
    budget: RunExecutionBudget,
    recorded_at: Instant,
}

/// per-run 预算表：run 启动时登记，随 run 结束（progress 快照过期）惰性回收。
struct RunBudgets {
    inner: RwLock<HashMap<String, BudgetEntry>>,
}

impl RunBudgets {
    fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    fn get(&self, run_id: &str) -> Option<RunExecutionBudget> {
        self.inner.read().get(run_id).map(|entry| entry.budget)
    }

    fn forget(&self, run_id: &str) {
        self.inner.write().remove(run_id);
    }
}

/// `agent/task` 发布面实现：关闭判定归任务表，maxAgents 准入与消费容量同源（预算表）。
struct RuntimeAgentTaskSink {
    tasks: Arc<WorkflowTaskTable>,
    budgets: Arc<RunBudgets>,
}

impl AgentTaskSink for RuntimeAgentTaskSink {
    fn publish_agent_task(&self, task_id: String, params: AgentRunParams) -> AgentTaskPublish {
        let run_id = params.run_id.clone();
        // 重复发布幂等：已登记的任务直接接受，不重复计入 maxAgents。
        if self.tasks.get(&run_id, &task_id).is_none() {
            if let Some(Some(maximum)) = self.budgets.get(&run_id).map(|budget| budget.max_agents) {
                let active = self.tasks.active_count_for_run(&run_id) as u64;
                if active >= maximum {
                    return AgentTaskPublish::LimitBreach(format!(
                        "workflow exceeded maxAgents ({maximum})"
                    ));
                }
            }
        }
        match self.tasks.publish(task_id, params) {
            PublishOutcome::Published(_) | PublishOutcome::AlreadyKnown(_) => {
                AgentTaskPublish::Accepted
            }
            PublishOutcome::Closed => AgentTaskPublish::Closed,
        }
    }
}

/// 取消路径的投递编码（design §5.3：`cancelled` 对齐 `Dead { reason }` 语义，
/// Node 侧按引擎约定抛 `WorkflowAbortedError`）。
fn cancelled_delivery_result(record: &WorkflowTaskRecord) -> AgentRunResult {
    AgentRunResult::Dead {
        reason: Some("killed".into()),
        detail: record
            .reason
            .clone()
            .or_else(|| Some("workflow task cancelled".into())),
    }
}

/// 编排领域控制接口：整合 runner / registry / progress / journal / 任务表 /
/// 事件日志 / 订阅广播。
pub struct WorkflowRuntime {
    runner: Arc<WorkflowRunner>,
    registry: Arc<WorkflowTaskRegistry>,
    progress_store: Arc<WorkflowProgressStore>,
    journal_store: Arc<WorkflowJournalStore>,
    bg_registry: RwLock<Option<Arc<dyn TaskManager>>>,
    tasks: Arc<WorkflowTaskTable>,
    run_events: Arc<WorkflowRunEventLog>,
    updates: broadcast::Sender<WorkflowResourceUpdate>,
    /// 消费者绑定令牌（`WorkflowAgentResult` 的调用授权；装配期一次绑定）。
    consumer_token: RwLock<Option<String>>,
    /// 消费者就绪位（MCP 面就绪屏障；旧链不经它判定）。
    consumer_ready: AtomicBool,
    /// 关闭门（关闭顺序第 ① 步：停止接受新 run 与新任务发布；任务表共享同一实例）。
    gate: Arc<CloseGate>,
    /// per-run 执行权威预算（maxConcurrency / limits；见 [`RunExecutionBudget`]）。
    budgets: Arc<RunBudgets>,
}

impl WorkflowRuntime {
    /// 创建运行时（含完整共享状态）。
    ///
    /// `notification_tx`：workflow 完成通知通道（ReAct 转发经旧链消费）。
    /// `progress_rx`：workflow 内部 agent 执行产生的进度事件（经 runner 转发）。
    pub fn new(
        agent_executor: Arc<dyn AgentExecutor>,
        cwd: &str,
        notification_tx: broadcast::Sender<WorkflowTaskResult>,
        progress_rx: Option<UnboundedReceiver<ProgressEvent>>,
    ) -> Self {
        let runner = Arc::new(WorkflowRunner::new(agent_executor, cwd, progress_rx));
        let registry = Arc::new(WorkflowTaskRegistry::new(notification_tx));
        let progress_store = Arc::new(WorkflowProgressStore::new());
        let journal_store = Arc::new(WorkflowJournalStore::new(cwd));
        let (updates, _) = broadcast::channel(UPDATE_CHANNEL_CAPACITY);
        let run_events = Arc::new(WorkflowRunEventLog::new(RUN_EVENT_LOG_CAPACITY));
        let gate = Arc::new(CloseGate::new());
        progress_store.set_observer(Arc::new(RuntimeProgressObserver::new(
            Arc::clone(&run_events),
            updates.clone(),
        )));
        let budgets = Arc::new(RunBudgets::new());
        let tasks = Arc::new(WorkflowTaskTable::new(updates.clone(), Arc::clone(&gate)));
        // P6 wire 面注入：`agent/task` 发布（关闭 / maxAgents 准入）与终态投递
        // （`agent/result`）。sink 持 Weak<WorkflowRunner>，不构成所有权环。
        runner.bind_agent_task_sink(Arc::new(RuntimeAgentTaskSink {
            tasks: Arc::clone(&tasks),
            budgets: Arc::clone(&budgets),
        }));
        let delivery_runner = Arc::downgrade(&runner);
        tasks.set_terminal_sink(Arc::new(move |record: &WorkflowTaskRecord| {
            let Some(runner) = delivery_runner.upgrade() else {
                return;
            };
            let result = record
                .result
                .clone()
                .unwrap_or_else(|| cancelled_delivery_result(record));
            // tool 计数与投递同点（任务终态先提交后投递）：计数只影响 run 终态投影，
            // 不改变已提交结果（design §5.2 第 5 条 limits 计费）。
            if let Some(count) = result.tool_count() {
                runner.note_task_tool_calls(&record.run_id, count);
            }
            runner.deliver_agent_result(&record.run_id, &record.task_id, &result);
        }));
        Self {
            runner,
            registry,
            progress_store,
            journal_store,
            bg_registry: RwLock::new(None),
            tasks,
            run_events,
            updates,
            consumer_token: RwLock::new(None),
            consumer_ready: AtomicBool::new(false),
            gate,
            budgets,
        }
    }

    pub fn runner(&self) -> &Arc<WorkflowRunner> {
        &self.runner
    }

    pub fn registry(&self) -> &Arc<WorkflowTaskRegistry> {
        &self.registry
    }

    pub fn progress_store(&self) -> &Arc<WorkflowProgressStore> {
        &self.progress_store
    }

    pub fn journal_store(&self) -> &Arc<WorkflowJournalStore> {
        &self.journal_store
    }

    pub fn tasks(&self) -> &Arc<WorkflowTaskTable> {
        &self.tasks
    }

    /// run 的执行容量（`maxConcurrency`）；未知 run 返回 `None`。
    ///
    /// 消费者据此排队：达到容量的任务留在 `published`，不进入 `executing`
    /// （design §5.2 第 5 条——容量与 limits 同一执行权威，不另立计数）。
    pub fn run_capacity(&self, run_id: &str) -> Option<u32> {
        self.budgets
            .get(run_id)
            .map(|budget| budget.max_concurrency)
    }

    /// 回填 agent 任务终态（`WorkflowAgentResult` 工具的领域入口）。
    ///
    /// 与任务表裸 `settle` 的唯一区别：`Ok` 结果缺 `phase` 时从 progress store 补注
    /// （engine 的 phase() 上下文只经 progress 事件传递、不进入 `AgentRunParams.phase`；
    /// 旧链在分派路径补注，P6 移到领域入口），其余单次终态提交语义不变。
    pub fn settle_agent_task(
        &self,
        run_id: &str,
        task_id: &str,
        state: WorkflowTaskState,
        result: Option<AgentRunResult>,
        reason: Option<String>,
    ) -> SettleOutcome {
        let result = result.map(|mut result| {
            if let AgentRunResult::Ok { phase, .. } = &mut result {
                if phase.is_none() {
                    if let Some(record) = self.tasks.get(run_id, task_id) {
                        *phase = self.progress_store.get_agent_phase(run_id, record.agent_id);
                    }
                }
            }
            result
        });
        self.tasks.settle(
            run_id,
            task_id,
            SettleAdmission::ExecutingOnly,
            state,
            result,
            reason,
        )
    }

    /// 登记 run 的执行权威预算；顺带惰性回收已结束 run 的条目。
    fn record_run_budget(&self, run_id: &str, input: &WorkflowInput) {
        let mut budgets = self.budgets.inner.write();
        budgets.retain(|id, entry| {
            // 启动窗口内（快照可能尚未出现）不得误删；窗口外以 progress 快照存续为准。
            entry.recorded_at.elapsed() < BUDGET_GRACE || self.progress_store.get_run(id).is_some()
        });
        budgets.insert(
            run_id.to_string(),
            BudgetEntry {
                budget: RunExecutionBudget {
                    max_concurrency: input.max_concurrency,
                    max_agents: input.limits.max_agents,
                    max_tool_calls: input.limits.max_tool_calls,
                },
                recorded_at: Instant::now(),
            },
        );
    }

    pub fn run_events(&self) -> &Arc<WorkflowRunEventLog> {
        &self.run_events
    }

    /// 订阅资源更新（唤醒信号；每订阅者一个 Receiver）。
    pub fn subscribe(&self) -> broadcast::Receiver<WorkflowResourceUpdate> {
        self.updates.subscribe()
    }

    /// 设置统一后台任务注册表（构造后链式调用）。
    pub fn with_bg_registry(self, bg_registry: Arc<dyn TaskManager>) -> Self {
        self.set_bg_registry(bg_registry);
        self
    }

    /// 延迟注入 bg_registry：已绑定不同实例时拒绝更换并记录（既有语义）。
    pub fn set_bg_registry(&self, bg_registry: Arc<dyn TaskManager>) {
        let mut current = self.bg_registry.write();
        if let Some(bound) = current.as_ref() {
            if !Arc::ptr_eq(bound, &bg_registry) {
                tracing::error!("Workflow execution manager cannot be replaced");
            }
            return;
        }
        if let Err(error) = self.runner.bind_execution_manager(Arc::clone(&bg_registry)) {
            tracing::error!(%error, "Workflow execution manager binding failed");
            return;
        }
        *current = Some(bg_registry);
    }

    /// 已绑定的执行管理器（`None` = 尚未绑定；绑定后不可替换，见 [`Self::set_bg_registry`]）。
    pub fn bg_registry(&self) -> Option<Arc<dyn TaskManager>> {
        self.bg_registry.read().clone()
    }

    /// 消费者就绪位（MCP 面屏障：就绪前 `Workflow` 工具拒绝新启动）。
    pub fn set_consumer_ready(&self, ready: bool) {
        self.consumer_ready.store(ready, Ordering::SeqCst);
    }

    pub fn consumer_ready(&self) -> bool {
        self.consumer_ready.load(Ordering::SeqCst)
    }

    /// 绑定消费者并返回调用令牌（幂等：已绑定则返回同一令牌）。
    ///
    /// 宿主消费者在装配期调用一次；`WorkflowAgentResult` 只接受携带该令牌的调用
    /// （design §4.2 调用授权：不信任调用方参数声明的身份）。
    pub fn bind_consumer_token(&self) -> String {
        let mut token = self.consumer_token.write();
        if let Some(existing) = token.as_ref() {
            return existing.clone();
        }
        let issued = uuid::Uuid::new_v4().to_string();
        *token = Some(issued.clone());
        issued
    }

    /// 校验调用方携带的消费者令牌。
    pub fn verify_consumer_token(&self, candidate: &str) -> bool {
        self.consumer_token
            .read()
            .as_ref()
            .is_some_and(|token| !candidate.is_empty() && token == candidate)
    }

    /// 消费者是否已绑定（未绑定时 `WorkflowAgentResult` 一律拒绝）。
    pub fn consumer_bound(&self) -> bool {
        self.consumer_token.read().is_some()
    }

    /// 标记关闭（关闭顺序第 ① 步：停止接受新 run 与新任务发布）。
    ///
    /// 返回后：不再放行新的 run 准入，run 注册与任务发布要么整体发生在关闭前，
    /// 要么被拒绝（在途准入持票期间本调用阻塞；`settle` / 取消不经此门）。
    pub fn mark_closed(&self) {
        self.gate.mark_closed();
    }

    pub fn is_closed(&self) -> bool {
        self.gate.is_closed()
    }

    /// 解析工具入参并启动（`Workflow` 工具的领域入口）。
    ///
    /// 单一权威：参数校验 / scriptPath / GitBaseline / resume 校验都在
    /// [`params::parse_start_input`]，旧 `WorkflowTool` 与 MCP 工具共用。
    pub async fn start_run(
        &self,
        input: serde_json::Value,
    ) -> Result<WorkflowStartOutcome, String> {
        let wf_input = params::parse_start_input(&input, self.runner.cwd())?;
        self.start_prepared_run(wf_input).await
    }

    /// 启动一个已准备好的 run（session 准入 + 自有完成提交）。
    ///
    /// 返回的 run_id 在调用方停止等待后仍然稳定（完成提交由注册的执行持有）。
    pub async fn start_prepared_run(
        &self,
        wf_input: WorkflowInput,
    ) -> Result<WorkflowStartOutcome, String> {
        // 关闭检查、执行准入与 run 注册在同一临界区（关闭门票据）：`mark_closed`
        // 等待在途准入结束，关闭返回后不再放行新 run（对抗审查 F6）；两个入口共用。
        let Some(ticket) = self.gate.admit() else {
            return Err("workflow instance is closed and no longer accepts new runs".into());
        };
        let owner = crate::completion::ExecutionOwner::admit(self.bg_registry.read().as_deref())?;
        let run_id = uuid::Uuid::now_v7().to_string();
        let workflow_name = wf_input.workflow_name.clone();
        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
        let started_at = peri_time::monotonic_now();

        // 先原子占用 registry 并发槽，再 spawn，避免并发失败产生孤儿 run。
        let script_preview: String = wf_input.script.chars().take(100).collect();
        self.registry
            .reserve(crate::registry::WorkflowRun {
                run_id: run_id.clone(),
                workflow_name: workflow_name.clone(),
                script_preview,
                status: crate::registry::WorkflowRunStatus::Running,
                started_at,
                child_handle: None,
                kill_tx: Some(kill_tx),
            })
            .map_err(|error| format!("Workflow concurrency limit: {error}"))?;
        // run 注册完成：准入临界区结束（票据在首个 await 前释放）。
        drop(ticket);
        // 执行权威预算先于 Node 进程登记：消费者的容量判定与 wire 准入（maxAgents）
        // 都依赖它（design §5.2 第 5 条）。
        self.record_run_budget(&run_id, &wf_input);

        // 注册到统一后台任务注册表（取消转发到 WorkflowTaskRegistry::kill 与任务表取消，
        // 与 `kill_run` 同一条路径：不留下「run 已杀、任务仍活」的悬挂）。
        let bg = self.bg_registry.read().clone();
        if let Some(ref bg) = bg {
            let kill_registry = Arc::clone(&self.registry);
            let kill_tasks = Arc::clone(&self.tasks);
            let kill_run_id = run_id.clone();
            if let Err(e) = bg.register(peri_acp_types::tasks::BgTaskRegistration {
                task_id: run_id.clone(),
                kind: peri_acp_types::tasks::BgTaskKind::Workflow,
                summary: format!(
                    "{}: {}",
                    workflow_name,
                    wf_input.script.chars().take(80).collect::<String>()
                ),
                pid: None,
                kill: Some(Box::new(move || {
                    let _ = kill_tasks.cancel_run(&kill_run_id, Some("workflow run killed".into()));
                    let _ = kill_registry.kill(&kill_run_id);
                })),
            }) {
                let _ = self.registry.kill(&run_id);
                self.budgets.forget(&run_id);
                return Err(format!("Workflow background admission failed: {e}"));
            }
        }

        let completion = crate::completion::RunCompletion {
            registry: Arc::downgrade(&self.registry),
            progress: Arc::clone(&self.progress_store),
            journal: Arc::clone(&self.journal_store),
            run_id: run_id.clone(),
            name: workflow_name.clone(),
            started_at,
        };
        let (child_handle, mut fast_rx) =
            match completion.spawn(Arc::clone(&self.runner), wf_input, kill_rx, bg, owner) {
                Ok(started) => started,
                Err(error) => {
                    let _ = self.registry.kill(&run_id);
                    self.budgets.forget(&run_id);
                    if let Some(bg) = self.bg_registry.read().as_ref() {
                        bg.confirm_external_execution_stopped(&run_id);
                        let _ = bg.cancel(&run_id);
                    }
                    return Err(error);
                }
            };
        self.registry.attach_child(&run_id, child_handle);

        // The one-second fast path observes the same already-published terminal projection.
        // Dropping this caller cannot cancel the registered execution/completion task.
        if let Ok(Some(completed)) = peri_time::timeout(
            std::time::Duration::from_secs(1),
            crate::completion::receive_completion(&mut fast_rx),
        )
        .await
        {
            if completed.result.status != crate::registry::WorkflowRunStatus::Completed {
                let error_msg = completed
                    .result
                    .error
                    .as_deref()
                    .unwrap_or("workflow failed with no error details");
                let detail = completed
                    .stderr_tail
                    .as_ref()
                    .map(|s| format!("\n\nstderr (last 20 lines):\n{}", s))
                    .unwrap_or_default();
                return Err(format!(
                    "Workflow '{}' failed: {}{}",
                    workflow_name, error_msg, detail
                ));
            }
        }

        let _ = self.updates.send(WorkflowResourceUpdate::RunsIndex);

        // run 终态收敛（design §5.3：不存在 detached 执行）：完成提交后取消残留任务
        // （迟到的取消由任务表幂等吸收），并回收执行权威预算。监视任务随 run 结束退出。
        let tasks_for_completion = Arc::clone(&self.tasks);
        let budgets_for_completion = Arc::clone(&self.budgets);
        let run_id_for_completion = run_id.clone();
        tokio::spawn(async move {
            if crate::completion::receive_completion(&mut fast_rx)
                .await
                .is_none()
            {
                return;
            }
            let cancelled = tasks_for_completion
                .cancel_run(&run_id_for_completion, Some("workflow run finished".into()));
            if !cancelled.is_empty() {
                tracing::debug!(
                    target: "workflow",
                    run_id = %run_id_for_completion,
                    cancelled = cancelled.len(),
                    "workflow run finished with non-terminal tasks; cancelled",
                );
            }
            budgets_for_completion.forget(&run_id_for_completion);
        });

        Ok(WorkflowStartOutcome {
            run_id,
            workflow_name,
        })
    }

    /// 终止 run（任务表取消 + registry kill），返回是否命中**运行中**的 run。
    ///
    /// 顺序固定：先在任务表内置 kill 栅栏并取消全部未终态任务（含并发到达的发布），
    /// 再向 registry 发 kill——kill 生效前发布的迟到任务命中栅栏，不逃逸出本轮 kill。
    /// 已完成 run（仅在 registry 留档）返回 `false`（不是「终止了运行中的 run」）。
    pub fn kill_run(&self, run_id: &str) -> bool {
        let cancelled = self
            .tasks
            .cancel_run(run_id, Some("workflow run killed".into()));
        let registry_killed = self.registry.kill(run_id).unwrap_or(false);
        !cancelled.is_empty() || registry_killed
    }

    /// 终止单个 agent（任务表取消 + 引擎 kill），返回是否命中。
    ///
    /// 与 [`Self::kill_run`] 同一条路径：先置 agent 级 kill 栅栏并取消该 agent 的
    /// 未终态任务，再向引擎发 kill（design §5.3「单 agent kill 与 run kill 走同一
    /// 任务表与取消句柄路径」）。
    pub async fn kill_agent(&self, run_id: &str, agent_id: u64) -> bool {
        let cancelled = !self
            .tasks
            .cancel_agent(run_id, agent_id, Some(format!("agent {agent_id} killed")))
            .is_empty();
        let engine = self.runner.kill_agent(run_id, agent_id).await;
        cancelled || engine
    }

    /// 恢复已完成的 workflow（GAP-04）：读 state.json 以 `resume_from` 重启。
    ///
    /// `run_id` 复用同一权威校验（[`params::is_safe_run_id`]）：ACP 入口与
    /// `resumeFromRunId` 参数走同一条规则，拒绝路径分隔符与非 UUID 目录名；
    /// 读取后校验 state 的 run_id 与请求一致（journal 目录按 run_id 命名，
    /// 不一致说明目录被替换或损坏）。
    pub async fn resume(&self, run_id: &str) -> Result<WorkflowStartOutcome, String> {
        if !params::is_safe_run_id(run_id) {
            return Err(format!(
                "Invalid resumeFromRunId '{}': must be a valid UUID without path traversal characters",
                run_id
            ));
        }
        let state = self
            .journal_store
            .read_state(run_id)
            .map_err(|e| format!("Failed to read workflow state: {e}"))?;
        if state.run_id != run_id {
            return Err(format!(
                "Workflow state mismatch: requested '{run_id}' but state.json belongs to '{}'",
                state.run_id
            ));
        }

        if state.status == "running" || state.status == "active" {
            return Err("Workflow is still running, cannot resume".into());
        }

        let write_intent = state.write_intent.clone();
        let git_baseline = crate::journal::GitBaseline::capture_for_intent(
            std::path::Path::new(self.runner.cwd()),
            write_intent.as_ref(),
        )
        .map_err(|error| format!("Workflow resume preflight failed: {error}"))?;
        let wf_input = WorkflowInput {
            script: state.script.clone(),
            args: state.args.clone(),
            max_concurrency: state.max_concurrency,
            budget_total: state.budget_total,
            limits: state.limits.clone(),
            workflow_name: state.workflow_name.clone(),
            resume_from: Some(run_id.to_string()),
            write_intent,
            git_baseline,
        };
        self.start_prepared_run(wf_input).await
    }

    /// run 索引快照（`workflow://runs`）。
    pub fn runs_snapshot(&self) -> Vec<RunProgress> {
        self.progress_store.list_runs()
    }

    /// run 事件水位（`workflow://runs/{runId}` 的 `eventWatermark`）：该 run 已提交的
    /// 最后一条事件 sequence；无提交时为 `0`。
    ///
    /// 水位单调不减：保留窗口淘汰事件后仍保持已提交值（比对「保留窗口内最新事件」
    /// 会让水位倒退，审查 F4）；可读缺口由 events 资源返回的 `truncatedBefore` 表达。
    pub fn event_watermark(&self, run_id: &str) -> u64 {
        self.run_events.committed_watermark(run_id)
    }

    /// 单 run 快照与其**一致**的已提交水位（`workflow://runs/{runId}` 的读取面）。
    ///
    /// 不变量：提交路径恒为「reducer 提交 → 事件追加 → 水位前进」（`progress::commit`
    /// 的串行提交器），因此**先取水位、后取快照**时，快照必包含水位内的全部提交；
    /// 读取方按 `after=watermark` 补读不会跳过事件。顺序颠倒（先快照后水位）会让
    /// 并发提交的 reducer 落入快照之前、事件落在水位之内而被跳过。
    pub fn run_snapshot_with_watermark(&self, run_id: &str) -> Option<(RunProgress, u64)> {
        let watermark = self.event_watermark(run_id);
        self.run_snapshot(run_id)
            .map(|snapshot| (snapshot, watermark))
    }

    /// 单 run 快照（`workflow://runs/{runId}`）。
    pub fn run_snapshot(&self, run_id: &str) -> Option<RunProgress> {
        self.progress_store.get_run(run_id)
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
