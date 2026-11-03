//! 进入会话时的恢复协调（设计 §4）：范围读取、single-flight 合并、逐 thread 复核、
//! 提醒接纳与延后启动。
//!
//! 进程内状态只有「正在准备的根」与「已接纳待启动的根」两张表：恢复不是持久记录，
//! 重启后不恢复旧执行。`enter` 只写 MQ（提醒）与内存表；`launch` 才放行执行，且调用方
//! 必须在本轮会话响应发出之后调用，避免恢复 turn 的通知先于响应。

use std::sync::Arc;

use dashmap::DashMap;
use peri_acp_types::ports::{McpPoolPort, PeripheralTasks};
use peri_acp_types::recovery::{
    RecoveryCandidate, RecoveryError, RecoveryErrorKind, RecoveryFailure, RecoveryReport,
    RecoveryScope, RecoverySkip, RecoverySkipReason, SessionRecovery,
};
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResources,
};
use peri_acp_types::thread::{ThreadId, WorkState};
use tokio::sync::Mutex;

use super::super::{activation, AcpServerConfig, SharedSessions};
use super::{reminder, scope};
use crate::session::executor::ContinuationRequest;
use crate::session::SessionManager;
use crate::transport::types::AcpError;
use peri_acp_types::tasks::BgTaskKind;

/// 根 thread 的恢复协调器；构造一次，由 `ServerLoop` 跨请求复用。
pub(crate) struct HostSessionRecovery {
    resources: Arc<dyn SessionResources>,
    manager: SessionManager,
    sessions: SharedSessions,
    cfg: Arc<AcpServerConfig>,
    cont_tx: Arc<tokio::sync::mpsc::UnboundedSender<ContinuationRequest>>,
    /// 正在准备的根 → 该根的串行锁；并发进入在同一把锁上排队。
    in_flight: DashMap<ThreadId, Arc<Mutex<()>>>,
    /// 已接纳提醒、等待响应之后启动的根。
    prepared: DashMap<ThreadId, PreparedLaunch>,
}

/// 启动门的快照：响应发出后再复核同一代次与同一打开态。
struct PreparedLaunch {
    generation: u64,
    session_id: String,
}

/// 一次生命周期快照；拿到即释放 `sessions` 锁。
#[derive(Clone, Copy)]
struct LifecycleSnapshot {
    generation: u64,
    closing: bool,
    turn_active: bool,
}

/// 单个 thread 的准备结论（根与冷 child 共用）。
enum ThreadOutcome {
    Reused,
    /// 已接纳提醒；携带按该 thread 直接 owner 查询到的外围任务事实。
    Injected(PeripheralTasks),
    Skipped(RecoverySkipReason),
    Failed(RecoveryFailure),
}

impl HostSessionRecovery {
    pub(crate) fn new(
        resources: Arc<dyn SessionResources>,
        manager: SessionManager,
        sessions: SharedSessions,
        cfg: Arc<AcpServerConfig>,
        cont_tx: Arc<tokio::sync::mpsc::UnboundedSender<ContinuationRequest>>,
    ) -> Self {
        Self {
            resources,
            manager,
            sessions,
            cfg,
            cont_tx,
            in_flight: DashMap::new(),
            prepared: DashMap::new(),
        }
    }

    /// 响应未能发出时丢弃本次启动资格：提醒留在 MQ，但本次不启动执行。
    pub(crate) fn discard(&self, report: &RecoveryReport) {
        for thread_id in &report.recovered {
            self.prepared.remove(thread_id);
        }
    }

    async fn load_scope(&self, thread_id: &ThreadId) -> Result<RecoveryScope, RecoveryError> {
        self.resources
            .load_recovery_scope(thread_id)
            .await
            .map_err(|error| RecoveryError::from(error).with_thread(thread_id))
    }

    async fn prepare(
        &self,
        scope: RecoveryScope,
        entered: &ThreadId,
    ) -> Result<RecoveryReport, RecoveryError> {
        let partition = scope::partition(scope);
        let root = partition.root_thread_id.clone();
        let entered_is_root = root == *entered;
        let mut report = RecoveryReport {
            root_thread_id: root.clone(),
            entered_thread_id: entered.clone(),
            ..Default::default()
        };

        match partition.root {
            None => report.skipped.push(RecoverySkip {
                thread_id: root.clone(),
                reason: RecoverySkipReason::Idle,
            }),
            Some(_) => match self.prepare_root(&partition, entered_is_root).await? {
                ThreadOutcome::Reused => report.reused.push(root.clone()),
                ThreadOutcome::Injected(peripheral) => {
                    report.peripheral_tasks = peripheral;
                    report.recovered.push(root.clone());
                }
                ThreadOutcome::Skipped(reason) => report.skipped.push(RecoverySkip {
                    thread_id: root.clone(),
                    reason,
                }),
                ThreadOutcome::Failed(failure) => report.failures.push(failure),
            },
        }

        // 逐 child 复核：已有运行实例的连接现有运行；冷 child 按 `session/load` 同一
        // 装配路径取得独立运行实例后自行接纳提醒——父级不是收件人，也不启动父级模型。
        // 外围任务按各 child 自己的直接 owner 查询，只写进它自己的提醒。
        for child in &partition.children {
            match self.prepare_child(&partition, child).await {
                ThreadOutcome::Reused => report.reused.push(child.thread_id.clone()),
                ThreadOutcome::Injected(_) => report.recovered.push(child.thread_id.clone()),
                ThreadOutcome::Skipped(reason) => report.skipped.push(RecoverySkip {
                    thread_id: child.thread_id.clone(),
                    reason,
                }),
                // child 的逐项失败不失败请求：装配/接纳失败按 Skip 报告（见 `prepare_child`）。
                ThreadOutcome::Failed(failure) => {
                    tracing::warn!(
                        thread_id = %failure.thread_id,
                        kind = failure.kind.as_str(),
                        detail = %failure.detail,
                        "recovery: child preparation failed"
                    );
                    report.skipped.push(RecoverySkip {
                        thread_id: failure.thread_id,
                        reason: RecoverySkipReason::AssemblyFailed,
                    });
                }
            }
        }

        self.record_failure_notice(entered, &report).await;
        Ok(report)
    }

    async fn prepare_root(
        &self,
        partition: &scope::RecoveryPartition,
        entered_is_root: bool,
    ) -> Result<ThreadOutcome, RecoveryError> {
        let root = &partition.root_thread_id;
        // 范围读取与复核之间可能已被收尾：只信刚读到的工作意图。
        let meta = match self.resources.load_session_meta(root).await {
            Ok(meta) => meta,
            Err(error) if entered_is_root => {
                return Err(RecoveryError::from(error).with_thread(root));
            }
            Err(error) => {
                return Ok(ThreadOutcome::Failed(RecoveryFailure {
                    thread_id: root.clone(),
                    kind: RecoveryErrorKind::Unavailable,
                    detail: error.to_string(),
                }));
            }
        };
        if meta.work_state != WorkState::Working {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::Idle));
        }

        // 根运行实例 = 本进程里的 Host 状态；没有会话记录就无法启动模型。
        let Some(session_id) = self.manager.session_id_for_thread(root) else {
            return Ok(ThreadOutcome::Failed(RecoveryFailure {
                thread_id: root.clone(),
                kind: RecoveryErrorKind::Unavailable,
                detail: "root session is not live in this process".to_owned(),
            }));
        };
        self.admit_thread(
            root,
            root,
            &session_id,
            &partition.direct_children_of(root),
            None,
            entered_is_root,
        )
        .await
    }

    /// 冷 child：没有本进程实例时按 `session/load` 同一路径装配，再按同一套复核
    /// 接纳它自己的提醒。父级只提供收件地址，绝不因 child 恢复而启动父级模型。
    ///
    /// 装配与接纳的失败都只记日志并按 Skip 报告，不失败请求（设计 §4.2）。
    async fn prepare_child(
        &self,
        partition: &scope::RecoveryPartition,
        child: &RecoveryCandidate,
    ) -> ThreadOutcome {
        let thread = &child.thread_id;
        // 父 turn 内的子 agent 已有执行登记：连接现有实例。
        if self.manager.thread_has_runtime(thread) {
            return ThreadOutcome::Reused;
        }
        let meta = match self.resources.load_session_meta(thread).await {
            Ok(meta) => meta,
            Err(error) => {
                tracing::warn!(thread_id = %thread, %error, "recovery: child meta read failed");
                return ThreadOutcome::Skipped(RecoverySkipReason::AssemblyFailed);
            }
        };
        match meta.work_state {
            WorkState::Working => {}
            WorkState::Idle => return ThreadOutcome::Skipped(RecoverySkipReason::Idle),
            WorkState::Stopped => return ThreadOutcome::Skipped(RecoverySkipReason::Stopped),
        }
        // 投递目标必须能由持久父链确定：没有直接父就如实报告，不猜收件人。
        if meta.parent_thread_id.is_none() {
            tracing::warn!(
                thread_id = %thread,
                "recovery: child has no parent thread; reminder not injected"
            );
            return ThreadOutcome::Skipped(RecoverySkipReason::AssemblyFailed);
        }

        let mut session_id = self.manager.session_id_for_thread(thread);
        if session_id.is_none() {
            // 失败也可能是并发装配刚完成：以现场实例再判一次，仍没有才算装配失败。
            if let Err(error) = self.assemble_child_session(thread).await {
                tracing::warn!(
                    thread_id = %thread,
                    code = error.code,
                    error = %error.message,
                    "recovery: cold child session assembly failed"
                );
            }
            session_id = self.manager.session_id_for_thread(thread);
        }
        let Some(session_id) = session_id else {
            return ThreadOutcome::Skipped(RecoverySkipReason::AssemblyFailed);
        };

        // 装配是异步准备：结束前可能已被收尾、停止或改挂父链，只信最终提交的意图。
        let current = match self.resources.load_session_meta(thread).await {
            Ok(meta) => meta,
            Err(error) => {
                tracing::warn!(thread_id = %thread, %error, "recovery: child meta re-read failed");
                return ThreadOutcome::Skipped(RecoverySkipReason::AssemblyFailed);
            }
        };
        match current.work_state {
            WorkState::Working => {}
            WorkState::Idle => return ThreadOutcome::Skipped(RecoverySkipReason::Idle),
            WorkState::Stopped => return ThreadOutcome::Skipped(RecoverySkipReason::Stopped),
        }
        let Some(parent) = current.parent_thread_id else {
            return ThreadOutcome::Skipped(RecoverySkipReason::AssemblyFailed);
        };

        let direct_children = partition.direct_children_of(thread);
        match self
            .admit_thread(
                &partition.root_thread_id,
                thread,
                &session_id,
                &direct_children,
                Some(parent),
                false,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(thread_id = %thread, %error, "recovery: child admission failed");
                ThreadOutcome::Skipped(RecoverySkipReason::AssemblyFailed)
            }
        }
    }

    /// 冷 child 的运行实例：与 `session/load` 同一条装配路径（不发会话响应）。
    async fn assemble_child_session(&self, thread: &ThreadId) -> Result<(), AcpError> {
        crate::host::requests::session_lifecycle::assemble_for_recovery(
            self.cfg.as_ref(),
            &self.sessions,
            thread.as_str(),
        )
        .await
    }

    /// 已装配 thread 的提醒准入：根与冷 child 共用同一条复核/注入序列。
    ///
    /// `fatal` 只对「进入的会话就是根」成立：此时失败必须成为请求错误；child 的
    /// 失败由调用方降级为 Skip。
    async fn admit_thread(
        &self,
        root: &ThreadId,
        thread: &ThreadId,
        session_id: &str,
        direct_children: &[ThreadId],
        parent: Option<ThreadId>,
        fatal: bool,
    ) -> Result<ThreadOutcome, RecoveryError> {
        let Some(before) = self.lifecycle(session_id).await else {
            return self.thread_failure(thread, "session state is not published", fatal);
        };
        if before.turn_active {
            // 已有实例正在执行本 thread：连接现有运行，不重复注入。
            return Ok(ThreadOutcome::Reused);
        }
        if before.closing {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::Stopped));
        }
        let Some(runtime) = self.manager.get_session(session_id) else {
            return self.thread_failure(thread, "session runtime is not registered", fatal);
        };
        let activation = Arc::clone(&runtime.activation);
        drop(runtime);
        // 显式 Stop 后不得经恢复重新放行执行。
        if activation.user_stopped() {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::Stopped));
        }

        let queue = self.queue_for(thread);
        if reminder::already_injected(&queue).await {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::AlreadyRunning));
        }

        // 注入前复核：等待根锁或读写期间会话可能已换代或个人已停止。
        let Some(current) = self.lifecycle(session_id).await else {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::Superseded));
        };
        if current.generation != before.generation {
            return Ok(ThreadOutcome::Skipped(RecoverySkipReason::Superseded));
        }
        if current.closing || current.turn_active {
            return Ok(ThreadOutcome::Skipped(if current.turn_active {
                RecoverySkipReason::AlreadyRunning
            } else {
                RecoverySkipReason::Stopped
            }));
        }

        // 设计 §6：外围任务只按本 thread 的直接 owner 查询。查询失败 / 未接线
        // 都是 Unknown，绝不写成「没有任务」；它也不阻断本 thread 的恢复。
        let peripheral_tasks = self.peripheral_tasks(session_id, thread).await;
        let trusted =
            match reminder::recovery_reminder(root, thread, direct_children, &peripheral_tasks) {
                Ok(trusted) => trusted,
                Err(detail) => {
                    return self.thread_failure(
                        thread,
                        format!("recovery reminder was rejected: {detail}"),
                        fatal,
                    )
                }
            };

        // 提醒接纳要能通过 `pending_request` 的激活门：放行执行资格在启动前最后一次复核。
        activation.allow();
        if let Err(error) = queue.enqueue(reminder::queued_recovery(trusted)).await {
            return self.thread_failure(thread, error.to_string(), fatal);
        }
        // 结果通知挂在会话实例上：本轮无论由启动门、激活监听还是用户 prompt 发起，
        // 收尾都能投递一次（不依赖哪一条续跑请求真正跑到）。
        if let Some(parent) = parent.clone() {
            if let Some(runtime) = self.manager.get_session(session_id) {
                runtime.record_recovery_parent(parent);
            }
        }
        self.prepared.insert(
            thread.clone(),
            PreparedLaunch {
                generation: before.generation,
                session_id: session_id.to_owned(),
            },
        );
        Ok(ThreadOutcome::Injected(peripheral_tasks))
    }

    /// 逐项失败分类：`fatal` 时上抛请求错误，否则只进报告。
    fn thread_failure(
        &self,
        thread: &ThreadId,
        detail: impl Into<String>,
        fatal: bool,
    ) -> Result<ThreadOutcome, RecoveryError> {
        let failure = RecoveryFailure {
            thread_id: thread.clone(),
            kind: RecoveryErrorKind::Unavailable,
            detail: detail.into(),
        };
        if fatal {
            return Err(self.fatal(thread, &failure));
        }
        Ok(ThreadOutcome::Failed(failure))
    }

    /// 本 thread 的外围存活任务查询（设计 §6，只读）。
    ///
    /// 会话环境里的池是会话装配产出的那份；宿主级装配没有池时回落到自身配置。
    /// 拿不到任何池就是 `Unknown`——这不是「没有任务」。
    async fn peripheral_tasks(&self, session_id: &str, owner: &ThreadId) -> PeripheralTasks {
        let session_pool = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(session_id)
                .and_then(|state| state.environment.as_ref())
                .and_then(|environment| environment.cfg.mcp_pool.clone())
        };
        let pool: Option<std::sync::Arc<dyn McpPoolPort>> =
            session_pool.or_else(|| self.cfg.mcp_pool.clone());
        let Some(pool) = pool else {
            return PeripheralTasks::unknown("session MCP pool is not live in this process");
        };
        pool.workspace_live_tasks(owner.as_str()).await
    }

    /// 逐项失败的诊断提醒：只记日志与 Tui/Automation 受众，不失败请求、不唤醒执行。
    async fn record_failure_notice(&self, entered: &ThreadId, report: &RecoveryReport) {
        if report.failures.is_empty() {
            return;
        }
        tracing::warn!(
            root_thread_id = %report.root_thread_id,
            failures = report.failures.len(),
            "session recovery partially failed"
        );
        let Ok(notice) = reminder::failure_notice(&report.root_thread_id, &report.failures) else {
            return;
        };
        let message = peri_acp_types::session::QueuedMessage::system_reminder(
            peri_acp_types::session::MessageKind::Info,
            peri_acp_types::session::MessageSource::SystemInjected,
            notice,
        );
        if let Err(error) = self.queue_for(entered).enqueue(message).await {
            tracing::warn!(%error, "session recovery failure notice was not admitted");
        }
    }

    fn fatal(&self, root: &ThreadId, failure: &RecoveryFailure) -> RecoveryError {
        RecoveryError::new(
            failure.kind,
            Some(root.clone()),
            SessionResourceError::new(SessionResourceErrorKind::Unavailable {
                detail: failure.detail.clone(),
            }),
        )
    }

    /// 根 thread 的 MQ：优先复用会话已有的门面（唤醒与监听共用），否则绑定同一 thread。
    fn queue_for(&self, thread_id: &ThreadId) -> MessageQueue {
        self.manager
            .session_id_for_thread(thread_id)
            .and_then(|session_id| self.manager.v2_queue_for(&session_id))
            .unwrap_or_else(|| MessageQueue::bind(Arc::clone(&self.resources), thread_id.clone()))
    }

    async fn lifecycle(&self, session_id: &str) -> Option<LifecycleSnapshot> {
        let sessions = self.sessions.lock().await;
        sessions.get(session_id).map(|state| LifecycleSnapshot {
            generation: state.session_generation,
            closing: state.closing,
            turn_active: state.cancel_token.is_some(),
        })
    }

    fn gate_for(&self, root: &ThreadId) -> Arc<Mutex<()>> {
        self.in_flight
            .entry(root.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// 只移除自己插入的那把锁：并发的下一次进入可能已建立新锁。
    fn release_gate(&self, root: &ThreadId, gate: &Arc<Mutex<()>>) {
        let same = self
            .in_flight
            .get(root)
            .is_some_and(|entry| Arc::ptr_eq(entry.value(), gate));
        if same {
            self.in_flight.remove(root);
        }
    }
}

#[async_trait::async_trait]
impl SessionRecovery for HostSessionRecovery {
    async fn enter(&self, thread_id: &ThreadId) -> Result<RecoveryReport, RecoveryError> {
        // 先解析根，再按根串行：同一根下不同 thread 的并发进入必须合并。
        let initial = self.load_scope(thread_id).await?;
        let root = initial.root_thread_id.clone();
        let gate = self.gate_for(&root);
        let guard = gate.lock().await;
        // 持锁重读：等待期间另一次进入可能已完成注入或启动。
        let prepared = match self.load_scope(thread_id).await {
            Ok(scope) => self.prepare(scope, thread_id).await,
            Err(error) => Err(error),
        };
        drop(guard);
        self.release_gate(&root, &gate);
        prepared
    }
}

impl HostSessionRecovery {
    /// 会话响应发出后放行执行：逐 thread `allow + ensure_listener + continuation`。
    ///
    /// 根与冷 child 各自启动自己的 turn；child 的续跑请求携带父 thread，turn 结束后
    /// 由 continuation scheduler 把结果通知投递回父级持久队列。
    pub(crate) async fn launch(&self, report: &RecoveryReport) {
        for thread_id in &report.recovered {
            let Some((_, prepared)) = self.prepared.remove(thread_id) else {
                continue;
            };
            self.launch_prepared(prepared).await;
        }
    }

    async fn launch_prepared(&self, prepared: PreparedLaunch) {
        let Some(state) = self.lifecycle(&prepared.session_id).await else {
            tracing::warn!(
                session_id = %prepared.session_id,
                "recovery launch skipped: session state disappeared"
            );
            return;
        };
        if state.generation != prepared.generation {
            tracing::warn!(
                session_id = %prepared.session_id,
                "recovery launch skipped: session generation changed"
            );
            return;
        }
        if state.closing || state.turn_active {
            tracing::warn!(
                session_id = %prepared.session_id,
                closing = state.closing,
                turn_active = state.turn_active,
                "recovery launch skipped: lifecycle moved on"
            );
            return;
        }
        let Some(runtime) = self.manager.get_session(&prepared.session_id) else {
            return;
        };
        let activation = Arc::clone(&runtime.activation);
        drop(runtime);
        // Stop 之后不再启动：提醒留在 MQ，由后续显式继续消费。
        if activation.user_stopped() || !activation.is_allowed() {
            tracing::debug!(
                session_id = %prepared.session_id,
                "recovery launch skipped: activation is not allowed"
            );
            return;
        }
        if let Err(error) = activation::ensure_listener(
            &prepared.session_id,
            &self.sessions,
            self.cfg.as_ref(),
            self.cont_tx.as_ref(),
        ) {
            tracing::warn!(
                session_id = %prepared.session_id,
                code = error.code,
                error = %error.message,
                "recovery launch skipped: listener could not start"
            );
            return;
        }
        if self
            .cont_tx
            .send(ContinuationRequest {
                session_id: prepared.session_id.clone(),
                kind: BgTaskKind::Agent,
                mq_steering: true,
                pending_result: false,
            })
            .is_err()
        {
            tracing::warn!(
                session_id = %prepared.session_id,
                "recovery launch skipped: continuation scheduler is gone"
            );
        }
    }
}
