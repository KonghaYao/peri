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
use peri_acp_types::thread::{ThreadId, ThreadMeta, WorkState};
use tokio::sync::Mutex;

use super::super::{AcpServerConfig, SharedSessions};
use super::{interrupted, reminder, runtime_binding, scope};
use crate::host::requests::session_lifecycle;
use crate::host::requests::session_lifecycle::RestorePublication;
use crate::session::executor::ContinuationRequest;
use crate::session::SessionManager;
use crate::transport::types::AcpError;

pub(crate) use host_session_recovery::HostSessionRecovery;

use host_session_recovery::AdmissionPhase;

use host_session_recovery::AdmittedRecovery;

use host_session_recovery::LifecycleSnapshot;

use host_session_recovery::ThreadOutcome;

pub(crate) use host_session_recovery::EnteredRecovery;

/// 仅测试：记录目标 root 的整树查询，验证空 child partition 不触碰存储树。
#[cfg(test)]
pub(crate) mod tree_query_probe {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use peri_acp_types::thread::ThreadId;

    static COUNTS: LazyLock<Mutex<HashMap<ThreadId, usize>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) fn install(root: &ThreadId) {
        COUNTS.lock().unwrap().insert(root.clone(), 0);
    }

    pub(super) fn record(root: &ThreadId) {
        if let Some(count) = COUNTS.lock().unwrap().get_mut(root) {
            *count += 1;
        }
    }

    pub(crate) fn take(root: &ThreadId) -> usize {
        COUNTS.lock().unwrap().remove(root).unwrap_or(0)
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
        /// 冷 child 已装配、装配后复核之前：构造装配期间目标变化或收尾的竞态。
        AfterColdAssembly,
        /// 冷 child 已准入、登记之前：构造父树删除快照先于登记的竞态。
        BeforeRecordRecoveredChild,
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

#[path = "coordinator/host_session_recovery.rs"]
mod host_session_recovery;
