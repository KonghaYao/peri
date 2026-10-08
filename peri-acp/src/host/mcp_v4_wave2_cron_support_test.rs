//! Shared cron test fixtures and observations for wave 2 test modules.

use super::*;
use peri_acp_types::{
    cron::CronSchedulerPort,
    event::ExecutorEvent,
    interaction::{
        ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
    },
    ports::McpTaskOwnerPort,
};
use peri_mcp_cron::{CronScheduler, CronSchedulerPortHandle, CronTrigger};
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    ModelStreamEvent, StopReason,
};
use serde_json::json;

pub(super) async fn collect_triggers(
    triggers: &mut tokio::sync::mpsc::UnboundedReceiver<peri_mcp_cron::CronTrigger>,
    window: std::time::Duration,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + window;
    let mut observed = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, triggers.recv()).await {
            Ok(Some(trigger)) => observed.push(trigger.task_id),
            // 发送端全关 / 窗口到点：两种都结束采集。
            Ok(None) | Err(_) => break,
        }
    }
    observed
}

pub(super) fn tool_end_events(sink: &MockEventSink) -> Vec<(String, String, bool)> {
    sink.operations()
        .iter()
        .filter_map(|operation| serde_json::from_str::<ExecutorEvent>(operation).ok())
        .filter_map(|event| match event {
            ExecutorEvent::ToolEnd {
                name,
                output,
                is_error,
                ..
            } => Some((name, output, is_error)),
            _ => None,
        })
        .collect()
}

/// 有界等待：条件成立返回 true，到点返回 false（由调用方给出断言文案）。
pub(super) async fn wait_until(
    label: &str,
    timeout: std::time::Duration,
    mut condition: impl FnMut() -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            println!("[W2 wait] {label}：等待 {timeout:?} 未成立");
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 清空在途触发后，在窗口内采集触发（窗口外的事件不属于本次计数）。
pub(super) async fn count_triggers_in_window(
    triggers: &mut tokio::sync::mpsc::UnboundedReceiver<CronTrigger>,
    window: std::time::Duration,
) -> Vec<String> {
    while triggers.try_recv().is_ok() {}
    collect_triggers(triggers, window).await
}

/// 强制到期后在有界窗口内采集触发（先排空在途：窗口外的事件不属于本次计数）。
pub(super) async fn force_due_and_collect(
    scheduler: &Arc<parking_lot::Mutex<CronScheduler>>,
    triggers: &mut tokio::sync::mpsc::UnboundedReceiver<CronTrigger>,
    task_id: &str,
    window: std::time::Duration,
) -> Vec<String> {
    assert!(
        scheduler.lock().force_next_fire_to_past(task_id),
        "强制到期前置：任务必须仍在组合根任务表内"
    );
    count_triggers_in_window(triggers, window).await
}

/// `rounds` 轮「强制到期 → 有界等待触发」，返回各轮计数。
///
/// 判据（可证伪）：**每一轮**窗口内都必须收到该任务的触发 —— 0 次就意味着这一代没有
/// tick 驱动（驱动挂掉 / reconnect 忘了挂新代），这正是「计数观测」要抓的失败形态。
pub(super) async fn force_due_rounds(
    label: &str,
    scheduler: &Arc<parking_lot::Mutex<CronScheduler>>,
    triggers: &mut tokio::sync::mpsc::UnboundedReceiver<CronTrigger>,
    task_id: &str,
    rounds: usize,
    window: std::time::Duration,
) -> Vec<usize> {
    let mut counts = Vec::new();
    for round in 1..=rounds {
        let observed = force_due_and_collect(scheduler, triggers, task_id, window).await;
        assert!(
            !observed.is_empty(),
            "{label} 第 {round} 轮：强制到期后 {window:?} 内必须出现触发（0 次 ⇒ 这一代没有 tick 驱动在跑）"
        );
        assert!(
            observed.iter().all(|id| id == task_id),
            "{label} 第 {round} 轮：触发必须来自该任务（排除其它路径送来的 trigger 冒充驱动证据）: {observed:?}"
        );
        counts.push(observed.len());
    }
    counts
}

pub(super) const W2_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// 1s 粒度的 cron 表达式（croner 支持可选秒字段）：每个 interval 都能重新到期，因此
/// 「窗口内触发次数」直接投影「tick 驱动次数」——这正是 H05 的计数观测面
/// （与 `peri-middlewares/src/mcp/builtin/cron_test.rs` 的计数窗口同口径）。
pub(super) const W2_EVERY_SECOND_CRON: &str = "* * * * * *";

/// 生产装配 + 真实 `run_initialize` 的宿主夹具（`drive_cron_tick` 可控）。
///
/// 与 [`BuiltinHostFixture`] 的差别只有一处，但这一处是本组用例的前提：pool / cron
/// scheduler / `SessionManager` 全部来自**同一次**生产装配
/// （`peri-acp/src/host/assemble.rs`），所以「cron 工具面、1s tick 驱动、宿主端口
/// `cfg.cron_scheduler`、session 级 cron bridge 订阅的是同一份 scheduler」是**构造事实**，
/// 不是用例的假设：用例再用一次 `downcast_arc::<CronSchedulerPortHandle>()`
/// （失败即 panic）把这条「同一份」钉死，然后才在它之上观测。
///
/// `drive_cron_tick` 是**既有装配开关**（TUI 路径 true；print/stdio false ⇒ 不挂 tick）。
pub(super) struct AssembledHostFixture {
    pub(super) dirs: FixtureDirs,
    pub(super) cfg: AcpServerConfig,
    pub(super) pool: Arc<McpClientPool>,
    pub(super) _owner: Box<dyn McpTaskOwnerPort>,
    /// 会话任务管理器登记（`session_context` 按生产语义绑定；必须活到用例结束，
    /// pool 只持 `Weak`）。
    pub(super) session_tasks: SessionTaskBindings,
}

impl AssembledHostFixture {
    pub(super) async fn start(dirs: FixtureDirs, drive_cron_tick: bool) -> Self {
        let mut cfg = assemble_host_with_tick(&dirs, drive_cron_tick).await;
        // 装配产出的唯一 MCP task owner：夹具必须持有到用例结束（task 归属在它手上）。
        let owner = cfg
            .mcp_task_owner
            .take()
            .expect("装配必须留下 MCP task owner");
        let pool = Arc::clone(&cfg.mcp_pool.clone().expect("装配必须构造 MCP pool"))
            .downcast_arc::<McpClientPool>()
            .unwrap_or_else(|_| panic!("装配的 MCP pool 必须是 McpClientPool"));
        // 真实 `run_initialize`（与 `run_acp_server` 同一步骤；上下文已由装配注入）。
        let (status_tx, mut status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        let init_pool = Arc::clone(&pool);
        let workspace = dirs.workspace.clone();
        let claude_home = dirs.claude_home.clone();
        let init_task = tokio::spawn(async move {
            McpClientPool::run_initialize(init_pool, &workspace, &claude_home, status_tx, None)
                .await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if matches!(
                    &*status_rx.borrow_and_update(),
                    McpInitStatus::Ready { .. } | McpInitStatus::Failed(_)
                ) {
                    break;
                }
                status_rx
                    .changed()
                    .await
                    .expect("init status 发送端在本任务内");
            }
        })
        .await
        .expect("真实 MCP 初始化不得挂起");
        tokio::time::timeout(std::time::Duration::from_secs(30), init_task)
            .await
            .expect("初始化任务必须在有界等待内结束")
            .expect("初始化任务不得 panic");
        let status = status_rx.borrow().clone();
        assert!(
            matches!(status, McpInitStatus::Ready { total: 4 }),
            "装配池必须 4/4 ready（否则 tick / 工具面 / 生命周期断言面全是空的）: {status:?}"
        );
        Self {
            dirs,
            cfg,
            pool,
            _owner: owner,
            session_tasks: SessionTaskBindings::default(),
        }
    }

    /// 组合根 cron 端口（`cfg.cron_scheduler`）：与 builtin 实例工具面同源的那一份。
    pub(super) fn cron_port(&self) -> Arc<dyn CronSchedulerPort> {
        Arc::clone(
            self.cfg
                .cron_scheduler
                .as_ref()
                .expect("生产装配必须注入 cron 端口（A1 组合根）"),
        )
    }

    /// 组合根 scheduler 本体（端口 downcast 的还原点）。
    ///
    /// downcast 失败必须 panic：「端口与实例工具面是同一份」是本组用例的地基，
    /// 不成立时后续所有观测都毫无意义（这正是 issue
    /// 2026-08-07-cron-tool-task-never-triggers 的形态）。
    pub(super) fn cron_scheduler(&self) -> Arc<parking_lot::Mutex<CronScheduler>> {
        let handle = Arc::clone(&self.cron_port())
            .downcast_arc::<CronSchedulerPortHandle>()
            .unwrap_or_else(|_| {
                panic!("cron 端口必须是 CronSchedulerPortHandle（否则工具面 / tick / bridge 不是同一份 scheduler）")
            });
        Arc::clone(&handle.0)
    }

    pub(super) fn assert_all_ready(&self) {
        for (instance, expected) in WAVE2_INSTANCES {
            assert_instance_ready(&self.pool, instance, expected);
        }
    }

    /// 注入本夹具 pool 的 prompt 装配面（与 [`BuiltinHostFixture::session_context`] 同形），
    /// 并按生产语义绑定本会话的任务管理器（`bind_session_tasks` 的会话侧一半）。
    pub(super) async fn session_context(&self, session_id: &str) -> SessionContext {
        let mut ctx = crate::host::executor_flow_tests::make_session_context(session_id).await;
        ctx.cwd = self.dirs.workspace_str();
        ctx.mcp_pool = Some(Arc::clone(&self.pool) as Arc<dyn McpPoolPort>);
        self.session_tasks.bind(&self.pool, &ctx);
        ctx
    }
}

/// 记录的审批项（工具名 + 入参）：审批**看到的名字**是独立事实（IF-D15 归一只作用于
/// 判定，事件载荷不归一）。
pub(super) type ApprovalRecord = (String, serde_json::Value);

/// 工具级审批 broker 替身：`cron_register` 的 mutation 审批走 `SessionContext.broker`
/// （不是 transport），与调度触发的审批是两个**互不干扰**的观测面。
pub(super) struct ToolApprovalBroker {
    pub(super) approve: bool,
    pub(super) requests: std::sync::atomic::AtomicUsize,
    pub(super) seen: parking_lot::Mutex<Vec<ApprovalRecord>>,
}

impl ToolApprovalBroker {
    pub(super) fn new(approve: bool) -> Self {
        Self {
            approve,
            requests: std::sync::atomic::AtomicUsize::new(0),
            seen: parking_lot::Mutex::new(Vec::new()),
        }
    }

    pub(super) fn requests(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(super) fn seen(&self) -> Vec<ApprovalRecord> {
        self.seen.lock().clone()
    }
}

#[async_trait::async_trait]
impl UserInteractionBroker for ToolApprovalBroker {
    async fn request(&self, context: InteractionContext) -> InteractionResponse {
        match context {
            InteractionContext::Approval { items } => {
                self.requests
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.seen.lock().extend(
                    items
                        .iter()
                        .map(|item| (item.tool_name.clone(), item.tool_input.clone())),
                );
                InteractionResponse::Decisions(
                    items
                        .iter()
                        .map(|_| {
                            if self.approve {
                                ApprovalDecision::Approve { source: None }
                            } else {
                                ApprovalDecision::Reject {
                                    reason: "w3 夹具拒绝".to_string(),
                                    source: None,
                                }
                            }
                        })
                        .collect(),
                )
            }
            _ => InteractionResponse::Rejected,
        }
    }
}

/// 审批应答 transport 替身：记录每一次 `session/request_permission` 的**逐字 params**
/// （审批调用计数与载荷都从这里取证），并按**当前**决策作答——决策可在用例中途翻转，
/// 因此批准 / 拒绝两条路径共用同一条真实链路（唯一变量是决策本身）。
pub(super) struct ApprovalTransport {
    pub(super) approve: std::sync::atomic::AtomicBool,
    pub(super) approvals: std::sync::atomic::AtomicUsize,
    response_gate: parking_lot::Mutex<Option<Arc<tokio::sync::Notify>>>,
    pub(super) requests: parking_lot::Mutex<Vec<serde_json::Value>>,
    pub(super) notifications: parking_lot::Mutex<Vec<String>>,
}

impl ApprovalTransport {
    pub(super) fn new(approve: bool) -> Self {
        Self {
            approve: std::sync::atomic::AtomicBool::new(approve),
            approvals: std::sync::atomic::AtomicUsize::new(0),
            response_gate: parking_lot::Mutex::new(None),
            requests: parking_lot::Mutex::new(Vec::new()),
            notifications: parking_lot::Mutex::new(Vec::new()),
        }
    }

    pub(super) fn set_approve(&self, approve: bool) {
        self.approve
            .store(approve, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) fn approvals(&self) -> usize {
        self.approvals.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(super) fn hold_next_response(&self) -> Arc<tokio::sync::Notify> {
        let gate = Arc::new(tokio::sync::Notify::new());
        *self.response_gate.lock() = Some(gate.clone());
        gate
    }

    pub(super) fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().clone()
    }

    pub(super) fn notifications(&self) -> Vec<String> {
        self.notifications.lock().clone()
    }
}

#[async_trait::async_trait]
impl AcpTransport for ApprovalTransport {
    async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, crate::transport::types::AcpError> {
        assert_eq!(
            method, "session/request_permission",
            "本替身只应答审批请求（其它服务端请求属于断言面之外的意外）"
        );
        let response_gate = self.response_gate.lock().take();
        self.approvals
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.requests.lock().push(params);
        if let Some(gate) = response_gate {
            gate.notified().await;
        }
        let option = if self.approve.load(std::sync::atomic::Ordering::SeqCst) {
            "allow_once"
        } else {
            "reject_once"
        };
        // ACP wire 形态（`peri-acp/src/broker/transport_broker.rs` 的解析入口）。
        Ok(json!({ "outcome": { "outcome": "selected", "optionId": option } }))
    }

    async fn send_notification(
        &self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<(), crate::transport::types::AcpError> {
        self.notifications.lock().push(method.to_string());
        Ok(())
    }

    async fn recv(&self) -> Option<crate::transport::types::IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: crate::transport::types::RequestId,
        _result: Result<serde_json::Value, crate::transport::types::AcpError>,
    ) -> Result<(), crate::transport::types::AcpError> {
        Ok(())
    }
}

/// 会话级脚本化模型：记录**每次请求的全部消息文本**（判据是「模型真的看到了 cron
/// reminder」，不是「turn 没报错」），只回一段收尾文本、不动任何工具面状态。
pub(super) struct TurnRecordingModel {
    pub(super) calls: std::sync::atomic::AtomicUsize,
    pub(super) requests: parking_lot::Mutex<Vec<String>>,
}

impl TurnRecordingModel {
    pub(super) fn new() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            requests: parking_lot::Mutex::new(Vec::new()),
        }
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(super) fn requests(&self) -> Vec<String> {
        self.requests.lock().clone()
    }
}

#[async_trait::async_trait]
impl Model for TurnRecordingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> ModelResult<ModelStream> {
        self.requests.lock().push(
            request
                .messages
                .iter()
                .map(|message| message.text_content().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let end = "w2 cron continuation done";
        let mut events = vec![ModelStreamEvent::TextDelta {
            text: end.to_string(),
        }];
        let response = ModelResponse::new(
            ModelMessage::assistant_text(end.to_string()),
            StopReason::EndTurn,
            None,
            None,
        )?;
        events.push(ModelStreamEvent::Completed(response));
        Ok(ModelStream::with_parent_cancellation(
            futures::stream::iter(events.into_iter().map(Ok)),
            cancellation,
        ))
    }
}
