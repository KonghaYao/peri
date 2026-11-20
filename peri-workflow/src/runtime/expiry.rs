//! 终态 run 的到期清扫：实例级可取消的定时任务。
//!
//! runner 退出时的清扫只覆盖刚结束的一轮；串行 workflow 的最后一个 run 结束后，
//! 若会话保持打开且没有后续 workflow 工作，含完整输出的进度快照会一直驻留到实例
//! 释放（内存审查 P1-B）。本任务按最近的终态到期时刻 `sleep_until` 后调用既有的
//! [`WorkflowProgressStore::cleanup_completed_at`]；没有终态 run 时只等唤醒信号，
//! 不保留定时器，也不轮询。

use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::progress::WorkflowProgressStore;

/// 终态 run 到期清扫器（每实例一个）。
///
/// 持 `Weak<WorkflowProgressStore>`：清扫器由 store 的观察者持有，强引用会形成
/// store ↔ 观察者环，实例释放后仍互相保活。
pub(super) struct ProgressExpirySweeper {
    progress: Weak<WorkflowProgressStore>,
    wake: Arc<Notify>,
    cancel: CancellationToken,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl ProgressExpirySweeper {
    pub(super) fn new(progress: Weak<WorkflowProgressStore>) -> Self {
        Self {
            progress,
            wake: Arc::new(Notify::new()),
            cancel: CancellationToken::new(),
            worker: Mutex::new(None),
        }
    }

    /// 登记一次到期检查：启动清扫任务（如未运行）并让它按当前快照重新计算。
    ///
    /// 只发唤醒信号、不做清扫：调用点在 store 的提交观察者内（持有 `commit`
    /// 串行锁），清扫必须留在独立任务里。
    pub(super) fn arm(&self) {
        let mut worker = self.worker.lock();
        // 实例已关闭：撤销后的唤醒不得再拉起任务。
        if self.cancel.is_cancelled() {
            return;
        }
        if worker.as_ref().is_none_or(JoinHandle::is_finished) {
            let Some(progress) = self.progress.upgrade() else {
                return;
            };
            // 同步构造面（`WorkflowRuntime::new` 与纯 `#[test]` 生命周期用例）可能
            // 不在 tokio 上下文内；生产提交路径都在任务里，总会启动。
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                return;
            };
            *worker = Some(handle.spawn(sweep(
                Arc::downgrade(&progress),
                Arc::clone(&self.wake),
                self.cancel.clone(),
            )));
        }
        self.wake.notify_one();
    }

    /// 撤销唤醒（实例关闭）：返回后清扫任务不再运行，也不再被登记唤醒。
    pub(super) fn shutdown(&self) {
        let mut worker = self.worker.lock();
        self.cancel.cancel();
        if let Some(worker) = worker.take() {
            worker.abort();
        }
    }
}

impl Drop for ProgressExpirySweeper {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(worker) = self.worker.get_mut().take() {
            worker.abort();
        }
    }
}

/// 单实例清扫循环：只在「有终态 run」时保留一个到期定时器。
async fn sweep(
    progress: Weak<WorkflowProgressStore>,
    wake: Arc<Notify>,
    cancel: CancellationToken,
) {
    loop {
        // 每轮重新升级：store 释放后自行退出，不延长实例生命周期。
        let deadline = match progress.upgrade() {
            Some(store) => store.next_terminal_expiry(),
            None => return,
        };
        let now = peri_time::monotonic_now();
        match deadline {
            // 没有终态 run：空闲等待，不安排空转定时器。
            None => {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    () = wake.notified() => {}
                }
            }
            // 已到期（例如登记时窗口已过）：立即清扫，下一轮重新计算。
            Some(deadline) if deadline <= now => {
                let Some(store) = progress.upgrade() else {
                    return;
                };
                store.cleanup_completed_at(now);
            }
            Some(deadline) => {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    // 新终态开窗或更早的到期登记：重新计算截止时刻。
                    () = wake.notified() => {}
                    () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {}
                }
            }
        }
    }
}
