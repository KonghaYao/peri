//! `WorkflowTaskConsumer` —— 宿主侧 agent 任务消费者（就绪、订阅、执行与回填接线）。
//!
//! 设计依据：`docs/design/workflow-mcp.md` §5.5（消费者）与 §4.3（资源与订阅的
//! 发现/补读协议）。会话环境持有本消费者（`host/workspace.rs`），生命周期与 session
//! 相同：
//!
//! 1. **就绪屏障**：在 builtin `workflow` 实例上建立 `workflow://tasks` 订阅并完成
//!    基线读取之后才置 `consumer_ready`；订阅失败 / 中断一律回落 not-ready——
//!    `Workflow` 工具在 not-ready 时拒绝新启动，避免「首个任务无人消费」。
//! 2. **消费**：通知只是唤醒信号（不携带正文）→ `resources/read` 重读任务基线
//!    （`activeTasks` = 未终态任务，重复扫描幂等）→ 对任务经任务表做**唯一 claim**
//!    （`mark_executing`，同临界区登记取消句柄）→ 用会话执行依赖运行 agent
//!    → 经 host-only 工具 `WorkflowAgentResult` 回填（`_meta` 携带消费者令牌）。
//!    有界并发取自 run 自身的执行权威（`maxConcurrency` + 任务表在执行数），不另立计数
//!    （design §5.2 第 5 条）。
//! 3. **关闭**：会话关闭取消消费者；在飞执行取消并有界等待，不残留 detached 执行。
//!
//! 边界：
//! - 任务发现经 MCP 资源面（与 wire 形态解耦；Node 侧 `agent/task` 发布在 P6 落地）；
//! - claim 是进程内任务表操作（任务表为 process-local 运行期状态，无对应 MCP 工具，
//!   见 design §5.1）；回填走 host 面工具，调用授权的单一权威在实例 handler；
//! - 执行依赖（executor）由会话装配面注入**与旧链同一实例**，不在此二次构造
//!   （P5 删除旧链后，本模块是该依赖的唯一持有者）。

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::ports::{McpPoolPort, McpResourceSubscription};
use peri_acp_types::workflow::{AgentExecutor, AgentRunParams};
use peri_mcp_workflow::{
    ClaimOutcome, WorkflowRuntime, WorkflowTaskRecord, TASKS_URI, WORKFLOW_CONSUMER_META_KEY,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// builtin workflow 实例名（池内寻址）。
const WORKFLOW_INSTANCE: &str = "workflow";
/// 回填工具名（host-only，`model_visible = false`）。
const AGENT_RESULT_TOOL: &str = "WorkflowAgentResult";
/// 订阅重建的初始退避。
const RESUBSCRIBE_INITIAL_BACKOFF: Duration = Duration::from_millis(200);
/// 订阅重建退避上界（实例初始化中 / 重连期间不空转）。
const RESUBSCRIBE_MAX_BACKOFF: Duration = Duration::from_secs(5);
/// 关闭时等待在飞执行收口的上界；超时强制中止并记录（design §5.5 第 ②/④ 步）。
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 回填投递的可重试次数（design §5.1：可重试投递 + 单次终态提交）。
const BACKFILL_ATTEMPTS: usize = 3;

/// `workflow://tasks` 正文的最小消费视图。
///
/// 只取发现所需的身份字段：`activeTasks` 是「未终态任务」基线（每次唤醒重读即幂等）；
/// 任务是否可执行由任务表的唯一 claim 裁决，**不**镜像状态机（状态单一权威在
/// `peri-workflow` 任务表）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskPage {
    #[serde(default)]
    active_tasks: Vec<TaskRef>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskRef {
    task_id: String,
    run_id: String,
}

/// 消费者句柄：持取消位与后台任务句柄；`stop` 取消并有界等待收口。
pub(crate) struct WorkflowTaskConsumer {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl WorkflowTaskConsumer {
    /// 启动消费者（订阅与就绪在后台异步完成；调用方不阻塞）。
    pub(crate) fn start(
        pool: Arc<dyn McpPoolPort>,
        runtime: Arc<WorkflowRuntime>,
        executor: Arc<dyn AgentExecutor>,
        session_id: String,
    ) -> Self {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            run(pool, runtime, executor, session_id, task_cancel).await;
        });
        Self { cancel, handle }
    }

    /// 停止消费者：取消订阅与在飞执行，有界等待后台任务收口（幂等）。
    pub(crate) async fn stop(mut self) {
        self.cancel.cancel();
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.handle)
            .await
            .is_err()
        {
            warn!(
                target: "workflow.consumer",
                "workflow task consumer did not drain in time; aborting"
            );
            self.handle.abort();
        }
    }
}

impl Drop for WorkflowTaskConsumer {
    fn drop(&mut self) {
        // 兜底：环境未走显式 shutdown 即被丢弃时，取消后台任务（不能 await 收口）。
        self.cancel.cancel();
    }
}

/// 消费者主循环：绑定令牌 → 订阅 → 消费 → 订阅终止后退避重建。
async fn run(
    pool: Arc<dyn McpPoolPort>,
    runtime: Arc<WorkflowRuntime>,
    executor: Arc<dyn AgentExecutor>,
    session_id: String,
    cancel: CancellationToken,
) {
    // 令牌绑定先于订阅：回填授权（`consumer_bound`）与就绪位（`consumer_ready`）是两条
    // 独立判定；绑定幂等（重复返回同一令牌）。
    let token = runtime.bind_consumer_token();
    let mut backoff = RESUBSCRIBE_INITIAL_BACKOFF;
    let mut announced = false;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match tokio::select! {
            biased;
            // 建立订阅可能等待实例侧就绪（有界）：会话关闭不等满超时。
            _ = cancel.cancelled() => break,
            subscription = pool.subscribe_instance_resource(WORKFLOW_INSTANCE, TASKS_URI) => {
                subscription
            }
        } {
            Ok(subscription) => {
                backoff = RESUBSCRIBE_INITIAL_BACKOFF;
                if announced {
                    info!(
                        target: "workflow.consumer",
                        session_id = %session_id,
                        "workflow task subscription re-established"
                    );
                }
                announced = false;
                drive(
                    &pool,
                    &runtime,
                    &executor,
                    &session_id,
                    &token,
                    subscription,
                    &cancel,
                )
                .await;
                // 订阅终止（实例重连 / 移除 / 池释放）：就绪位回落，重建后再置位。
                runtime.set_consumer_ready(false);
            }
            Err(error) => {
                runtime.set_consumer_ready(false);
                if !announced {
                    announced = true;
                    warn!(
                        target: "workflow.consumer",
                        session_id = %session_id,
                        error = %error,
                        "workflow task subscription unavailable; retrying"
                    );
                } else {
                    debug!(
                        target: "workflow.consumer",
                        session_id = %session_id,
                        error = %error,
                        "workflow task subscription still unavailable"
                    );
                }
            }
        }
        if !sleep_or_cancel(&cancel, backoff).await {
            break;
        }
        backoff = (backoff * 2).min(RESUBSCRIBE_MAX_BACKOFF);
    }
    runtime.set_consumer_ready(false);
    debug!(target: "workflow.consumer", session_id = %session_id, "workflow task consumer stopped");
}

/// 单条订阅的生命周期：基线 → 置就绪 → 增量唤醒 → 收口在飞执行。
async fn drive(
    pool: &Arc<dyn McpPoolPort>,
    runtime: &Arc<WorkflowRuntime>,
    executor: &Arc<dyn AgentExecutor>,
    session_id: &str,
    token: &str,
    mut subscription: McpResourceSubscription,
    cancel: &CancellationToken,
) {
    let mut inflight: JoinSet<()> = JoinSet::new();

    // 订阅确认后立即 read 建立基线（design §4.3 第 1 步）。基线失败不置就绪：
    // 句柄随本函数返回 drop（注销订阅），由外层退避重建。
    if let Err(error) = scan(pool, runtime, executor, token, &mut inflight, cancel).await {
        warn!(
            target: "workflow.consumer",
            session_id,
            error = %error,
            "workflow task baseline read failed; consumer stays not-ready"
        );
        return;
    }
    runtime.set_consumer_ready(true);
    info!(
        target: "workflow.consumer",
        session_id,
        "workflow task consumer ready"
    );

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            wake = subscription.next() => match wake {
                Some(uri) if uri == TASKS_URI => {
                    if let Err(error) = scan(
                        pool, runtime, executor, token, &mut inflight, cancel,
                    )
                    .await
                    {
                        // 通知只是唤醒信号：单次读取失败不改变就绪位，下一次唤醒重试。
                        warn!(
                            target: "workflow.consumer",
                            session_id,
                            error = %error,
                            "workflow task scan failed; next wake-up retries"
                        );
                    }
                }
                Some(_) => {}
                None => break,
            },
            Some(joined) = inflight.join_next(), if !inflight.is_empty() => {
                if let Err(error) = joined {
                    warn!(
                        target: "workflow.consumer",
                        session_id,
                        error = %error,
                        "workflow task execution panicked"
                    );
                }
            }
        }
    }

    // 收口：在飞执行的子令牌随父取消传导；有界等待，超时强制中止并记录。
    if tokio::time::timeout(DRAIN_TIMEOUT, async {
        while inflight.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        warn!(
            target: "workflow.consumer",
            session_id,
            inflight = inflight.len(),
            "workflow task executions did not drain in time; aborting"
        );
        inflight.abort_all();
        while inflight.join_next().await.is_some() {}
    }
}

/// 读取任务基线并对可 claim 的任务登记执行（幂等：重复扫描由唯一 claim 吸收）。
async fn scan(
    pool: &Arc<dyn McpPoolPort>,
    runtime: &Arc<WorkflowRuntime>,
    executor: &Arc<dyn AgentExecutor>,
    token: &str,
    inflight: &mut JoinSet<()>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let payload = pool
        .read_instance_resource(WORKFLOW_INSTANCE, TASKS_URI)
        .await?;
    let page: TaskPage = serde_json::from_str(&payload)
        .map_err(|error| format!("invalid {TASKS_URI} payload: {error}"))?;
    for task in page.active_tasks {
        if cancel.is_cancelled() {
            break;
        }
        // 有界并发 = run 自身的执行权威（design §5.2 第 5 条）：容量取自 run
        // `maxConcurrency`，在执行数取自任务表（单一真相，不另立计数）。达到上限的
        // 任务留在 `published`（排队），由后续唤醒处理。
        if let Some(capacity) = runtime.run_capacity(&task.run_id) {
            let executing = runtime.tasks().executing_count_for_run(&task.run_id);
            if executing >= capacity as usize {
                continue;
            }
        } else {
            // 预算缺失是装配异常（run 未登记）或测试构造：不做本地限制并记录——
            // 不引入第二个计数口径，也不因本地判定阻塞任务（liveness 优先）。
            debug!(
                target: "workflow.consumer",
                run_id = %task.run_id,
                task_id = %task.task_id,
                "workflow run has no execution budget; capacity gate skipped",
            );
        }
        let task_cancel = cancel.child_token();
        // 其余 claim 结果（已被 claim / 已终态 / 已不存在）不执行：唯一 claim 的语义——
        // 调用方不得执行。
        if let ClaimOutcome::Claimed(record) =
            runtime
                .tasks()
                .mark_executing(&task.run_id, &task.task_id, task_cancel.clone())
        {
            let pool = Arc::clone(pool);
            let executor = Arc::clone(executor);
            let token = token.to_string();
            inflight.spawn(async move {
                execute_and_backfill(pool, executor, token, record, task_cancel).await;
            });
        }
    }
    Ok(())
}

/// 执行单个任务并回填结果（`WorkflowAgentResult`，`_meta` 携带消费者令牌）。
async fn execute_and_backfill(
    pool: Arc<dyn McpPoolPort>,
    executor: Arc<dyn AgentExecutor>,
    token: String,
    record: WorkflowTaskRecord,
    task_cancel: CancellationToken,
) {
    let run_id = record.run_id.clone();
    let task_id = record.task_id.clone();
    let params: AgentRunParams = record.params.clone();
    let result = tokio::select! {
        biased;
        // kill / 会话关闭：任务表已由取消路径提交 `cancelled` 终态（唯一提交点），
        // 执行侧只负责停止，不重复结算（迟到回填由任务表幂等吸收）。
        _ = task_cancel.cancelled() => {
            debug!(
                target: "workflow.consumer",
                run_id = %run_id,
                task_id = %task_id,
                "workflow task cancelled before completion"
            );
            return;
        }
        result = executor.execute(params) => result,
    };
    let result_value = match serde_json::to_value(&result) {
        Ok(value) => value,
        Err(error) => {
            warn!(
                target: "workflow.consumer",
                run_id = %run_id,
                task_id = %task_id,
                error = %error,
                "workflow task result is not serializable; result not delivered"
            );
            return;
        }
    };
    let arguments = json!({ "runId": run_id, "taskId": task_id, "result": result_value });
    let mut meta = Map::new();
    meta.insert(WORKFLOW_CONSUMER_META_KEY.to_string(), Value::String(token));
    let mut last_error = String::new();
    for attempt in 1..=BACKFILL_ATTEMPTS {
        match pool
            .call_instance_tool(
                WORKFLOW_INSTANCE,
                AGENT_RESULT_TOOL,
                arguments.clone(),
                Some(Value::Object(meta.clone())),
            )
            .await
        {
            Ok(_) => {
                debug!(
                    target: "workflow.consumer",
                    run_id = %run_id,
                    task_id = %task_id,
                    "workflow task result delivered"
                );
                return;
            }
            Err(error) => {
                last_error = error;
                if attempt < BACKFILL_ATTEMPTS {
                    sleep_or_cancel(&task_cancel, Duration::from_millis(250 * attempt as u64))
                        .await;
                    if task_cancel.is_cancelled() {
                        return;
                    }
                }
            }
        }
    }
    warn!(
        target: "workflow.consumer",
        run_id = %run_id,
        task_id = %task_id,
        error = %last_error,
        "workflow task result delivery failed after retries; task remains executing \
         until the run is cancelled (kill / session close paths still settle it)"
    );
}

/// 可取消的退避等待；返回 `false` = 已取消。
async fn sleep_or_cancel(cancel: &CancellationToken, backoff: Duration) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(backoff) => true,
    }
}
