//! 进入会话时的恢复协调（设计 §4）：范围读取、single-flight 合并、逐 thread 复核、
//! 提醒接纳与延后启动。
//!
//! 进程内状态只有「正在准备的根」与「已接纳待启动的根」两张表：恢复不是持久记录，
//! 重启后不恢复旧执行。`enter` 只写 MQ（提醒）与内存表；`launch` 才放行执行，且调用方
//! 必须在本轮会话响应发出之后调用，避免恢复 turn 的通知先于响应。

mod admission;
mod launch;

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use peri_acp_types::ports::PeripheralTasks;
use peri_acp_types::recovery::{
    RecoveryCandidate, RecoveryError, RecoveryErrorKind, RecoveryFailure, RecoveryReport,
    RecoveryScope, RecoverySkip, RecoverySkipReason, SessionRecovery,
};
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::thread::{ThreadId, WorkState};
use tokio::sync::Mutex;

use super::super::{AcpServerConfig, SharedSessions};
use super::{interrupted, reminder, runtime_binding, scope};
use crate::session::executor::ContinuationRequest;
use crate::session::SessionManager;
use crate::transport::types::AcpError;

/// 根 thread 的恢复协调器；构造一次，由 `ServerLoop` 跨请求复用。
pub(crate) struct HostSessionRecovery {
    resources: Arc<dyn SessionResources>,
    manager: SessionManager,
    sessions: SharedSessions,
    cfg: Arc<AcpServerConfig>,
    cont_tx: Arc<tokio::sync::mpsc::UnboundedSender<ContinuationRequest>>,
    /// 正在准备的根 → 该根的串行锁；并发进入在同一把锁上排队。
    ///
    /// 注册表本身也加锁：移除判定必须与「取锁 + 克隆」线性化，否则清点后被
    /// 新进入者克隆再删表，顺序会退化成两把并行的锁。
    in_flight: parking_lot::Mutex<HashMap<ThreadId, Arc<Mutex<()>>>>,
    /// 已接纳提醒的 thread：进程内唯一防重复事实（需要覆盖 `discard` 后的复用）。
    admitted: DashMap<ThreadId, AdmittedRecovery>,
}

/// 一次准入的生命周期：提醒已入队（`Prepared`）与执行已放行（`Launched`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionPhase {
    Prepared,
    Launched,
}

/// 启动门的快照：响应发出后再复核同一代次与同一打开态。
#[derive(Debug, Clone)]
struct AdmittedRecovery {
    generation: u64,
    session_id: String,
    phase: AdmissionPhase,
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
    /// 已接纳提醒；携带按该 thread 直接 owner 查询到的外围任务事实与本代次准入。
    Injected {
        peripheral_tasks: PeripheralTasks,
        admission: AdmittedRecovery,
    },
    Skipped(RecoverySkipReason),
    Failed(RecoveryFailure),
}

/// 一次 `enter` 的结果与它实际写入的准入快照。
///
/// `discard` 必须按这份快照匹配，不能只凭 thread id 猜测：并发进入可能已经写下
/// 更新代次的准入，旧响应失败不得把它删掉。
#[derive(Debug)]
pub(crate) struct EnteredRecovery {
    pub(crate) report: RecoveryReport,
    admissions: Vec<(ThreadId, AdmittedRecovery)>,
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
            in_flight: parking_lot::Mutex::new(HashMap::new()),
            admitted: DashMap::new(),
        }
    }

    /// 响应未能发出时丢弃本次启动资格：提醒留在 MQ，下一次进入可采纳并重新启动。
    ///
    /// 只移除本次 `enter` 自己写入、且仍是 `Prepared` 的准入：`Launched` 已有本进程
    /// 执行证据，更新代次留下的准入也不能因旧报告被删。
    pub(crate) fn discard(&self, entered: &EnteredRecovery) {
        for (thread_id, admission) in &entered.admissions {
            self.admitted.remove_if(thread_id, |_, entry| {
                entry.generation == admission.generation
                    && entry.session_id == admission.session_id
                    && entry.phase == AdmissionPhase::Prepared
            });
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
    ) -> Result<EnteredRecovery, RecoveryError> {
        let partition = scope::partition(scope);
        let root = partition.root_thread_id.clone();
        let entered_is_root = root == *entered;
        let mut report = RecoveryReport {
            root_thread_id: root.clone(),
            entered_thread_id: entered.clone(),
            ..Default::default()
        };
        let mut admissions = Vec::new();

        match partition.root {
            None => report.skipped.push(RecoverySkip {
                thread_id: root.clone(),
                reason: RecoverySkipReason::Idle,
            }),
            Some(_) => match self.prepare_root(&partition, entered_is_root).await? {
                ThreadOutcome::Reused => report.reused.push(root.clone()),
                ThreadOutcome::Injected {
                    peripheral_tasks,
                    admission,
                } => {
                    report.peripheral_tasks = peripheral_tasks;
                    report.recovered.push(root.clone());
                    admissions.push((root.clone(), admission));
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
                ThreadOutcome::Injected { admission, .. } => {
                    report.recovered.push(child.thread_id.clone());
                    admissions.push((child.thread_id.clone(), admission));
                }
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
        Ok(EnteredRecovery { report, admissions })
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
        // 父 turn 内的子 agent 已有执行登记：连接现有实例；若它是恢复 child 且
        // 直接父级此时已 live，则补做 adoption（不启动新执行）。
        if self.manager.thread_has_runtime(thread) {
            self.adopt_live(thread).await;
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

    /// 直接父级之后进入时，为已在运行的恢复 child 补登记父任务/取消关系。
    async fn adopt_live(&self, thread: &ThreadId) {
        let Some(session_id) = self.manager.session_id_for_thread(thread) else {
            return;
        };
        runtime_binding::adopt_recovered_child(self.cfg.as_ref(), &self.sessions, &session_id)
            .await;
    }

    fn gate_for(&self, root: &ThreadId) -> Arc<Mutex<()>> {
        self.in_flight
            .lock()
            .entry(root.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// 只移除已无等待者的锁：map + 本地各一份时才算独占。
    ///
    /// 等待者仍持有 Arc 时删表，下一次进入会新建另一把锁并并行执行。
    /// 判定与 [`Self::gate_for`] 共用注册表锁，避免清点后又被新进入者克隆。
    fn release_gate(&self, root: &ThreadId, gate: &Arc<Mutex<()>>) {
        let mut in_flight = self.in_flight.lock();
        let Some(entry) = in_flight.get(root) else {
            return;
        };
        let exclusive = Arc::strong_count(entry) == 2 && Arc::ptr_eq(entry, gate);
        if exclusive {
            in_flight.remove(root);
        }
    }

    #[cfg(test)]
    pub(crate) fn in_flight_gate(&self, root: &ThreadId) -> Option<Arc<Mutex<()>>> {
        self.in_flight.lock().get(root).cloned()
    }

    #[cfg(test)]
    pub(crate) fn in_flight_gate_count(&self, root: &ThreadId) -> usize {
        self.in_flight.lock().get(root).map_or(0, Arc::strong_count)
    }
}

#[async_trait::async_trait]
impl SessionRecovery for HostSessionRecovery {
    async fn enter(&self, thread_id: &ThreadId) -> Result<RecoveryReport, RecoveryError> {
        self.enter_prepared(thread_id)
            .await
            .map(|entered| entered.report)
    }
}

impl HostSessionRecovery {
    /// 与 trait [`SessionRecovery::enter`] 同一流程，额外返回本次写入的准入快照：
    /// 响应失败时 `discard` 必须按这份快照匹配，不能删除其他并发进入的准入。
    pub(crate) async fn enter_prepared(
        &self,
        thread_id: &ThreadId,
    ) -> Result<EnteredRecovery, RecoveryError> {
        // 先解析根，再按根串行：同一根下不同 thread 的并发进入必须合并。
        let initial = self.load_scope(thread_id).await?;
        let root = initial.root_thread_id.clone();
        let gate = self.gate_for(&root);
        let guard = gate.lock().await;
        #[cfg(test)]
        admission_gate::wait(root.as_str(), admission_gate::PausePoint::InsideGate).await;
        // 进入根时先收敛遗留 Active 的 child（非根、非活实例、close 已结清）：
        // 这是显式进入的副作用，不续跑、不注提醒；必须在权威范围重读之前完成。
        if root == *thread_id {
            interrupted::settle_interrupted_children(
                &self.resources,
                &self.manager,
                &self.sessions,
                &root,
            )
            .await;
        }
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

/// 仅测试：把恢复准备暂停在指定窗口，构造 Stop 与并发进入的真实竞态。
///
/// 生产路径只多一次 `cfg(test)` 查表，不引入运行时状态。
#[cfg(test)]
pub(crate) mod admission_gate {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, LazyLock, Mutex};
    use tokio::sync::{Notify, Semaphore};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub(crate) enum PausePoint {
        /// 已取得根锁：后续进入必须排在同一个门后。
        InsideGate,
        /// 外围查询与生命周期复核之后、恢复放行之前。
        BeforeAdmit,
    }

    pub(crate) struct Gate {
        arrived: AtomicUsize,
        entered: Notify,
        released: Semaphore,
    }

    impl Gate {
        fn new() -> Self {
            Self {
                arrived: AtomicUsize::new(0),
                entered: Notify::new(),
                released: Semaphore::new(0),
            }
        }

        pub(crate) fn arrived(&self) -> usize {
            self.arrived.load(Ordering::Acquire)
        }

        /// 等到至少 `count` 次准备停在这道门上。
        pub(crate) async fn wait_arrived(&self, count: usize) {
            loop {
                let notified = self.entered.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.arrived() >= count {
                    return;
                }
                notified.await;
            }
        }

        /// 放行 `permits` 次等待。
        pub(crate) fn release(&self, permits: usize) {
            self.released.add_permits(permits);
        }
    }

    type GateRegistry = Mutex<HashMap<(String, PausePoint), Arc<Gate>>>;

    static GATES: LazyLock<GateRegistry> = LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) fn install(thread_id: &str, point: PausePoint) -> Arc<Gate> {
        let gate = Arc::new(Gate::new());
        GATES
            .lock()
            .unwrap()
            .insert((thread_id.to_owned(), point), Arc::clone(&gate));
        gate
    }

    pub(crate) fn uninstall(thread_id: &str, point: PausePoint) {
        GATES.lock().unwrap().remove(&(thread_id.to_owned(), point));
    }

    pub(crate) async fn wait(thread_id: &str, point: PausePoint) {
        let gate = GATES
            .lock()
            .unwrap()
            .get(&(thread_id.to_owned(), point))
            .cloned();
        let Some(gate) = gate else {
            return;
        };
        gate.arrived.fetch_add(1, Ordering::AcqRel);
        gate.entered.notify_waiters();
        let permit = gate
            .released
            .acquire()
            .await
            .expect("test admission gate stays open");
        // forget：消费许可而不是归还，一次 release 只放行一次。
        permit.forget();
    }
}
