//! One bounded command queue: admission, oldest-event replacement, and closing
//! share a synchronous commit boundary. Capacity waiters are owned by Tokio.
//!
//! 队列只保存有界来源/引用：每条 `Add` 持有 [`crate::snapshot::TelemetryEvent`]（事件原文 permit
//! 与 input/output 来源句柄），不保存完整正文；文件引用按描述内存计费（[`queued_bytes`]）。
//!
//! 冻结准入（[`Admission::try_freeze`]）先取有界**准备登记槽**，再取 retained 额度，最后分配
//! `prepare_seq` 并登记准备任务——空闲槽不足立即拒收，不留下半登记，也不以等待 permit 的任务
//! 列表保存大对象。`event_seq`/`prepare_seq` 在同一个 admission 锁内各自单调递增。

use std::{
    collections::VecDeque,
    sync::{atomic::AtomicBool, Arc, Mutex},
};

use tokio::sync::{watch, Notify, OwnedSemaphorePermit, Semaphore, TryAcquireError};

use super::budget::queued_bytes;
use super::{BatcherCommand, QueuedEvent};
use crate::config::{BackpressurePolicy, BatcherConfig};
use crate::memory::{MemoryBudget, MemoryPermit};
use crate::snapshot::{SnapshotCell, SnapshotSource, SnapshotState, SnapshotTicket};
use crate::{LangfuseError, TelemetryEvent};

pub(super) struct Admission {
    shared: Arc<SharedQueue>,
}

pub(super) struct CommandReceiver {
    shared: Arc<SharedQueue>,
}

/// 一条已登记、等待唯一 worker 执行的准备任务。
///
/// 登记槽 permit 随请求存活到任务终结：worker 取走请求即接手该槽，完成/失败/取消后随之释放，
/// 因此「活跃快照数」始终受 `queue_capacity` 约束，不存在无界准备队列。
pub(super) struct PrepareRequest {
    pub(super) cell: Arc<SnapshotCell>,
    pub(super) source: Arc<dyn SnapshotSource>,
    /// 输出 allowance：Inline 完成转成 `BudgetedBytes` 的 permit，失败/取消时归还。
    pub(super) allowance: MemoryPermit,
    /// 源 retained 额度：完成（内容已转成快照字节或文件引用）后释放。
    pub(super) retained: MemoryPermit,
    _slot: OwnedSemaphorePermit,
}

struct SharedQueue {
    state: Mutex<QueueState>,
    slots: Arc<Semaphore>,
    /// 有界准备登记槽（契约 §4：活跃快照 slot 数受 `queue_capacity` 限制）。
    prepare_slots: Arc<Semaphore>,
    prepare: Mutex<VecDeque<PrepareRequest>>,
    ready: Notify,
    space: Notify,
    /// 准备任务登记/完成唤醒点；`Arc` 让 worker 的 select 分支不必借用 receiver 本体。
    prepare_progress: Arc<Notify>,
    max_event_bytes: usize,
    max_queue_bytes: usize,
    closing: watch::Sender<bool>,
}

struct QueueState {
    accepting: bool,
    commands: VecDeque<QueuedCommand>,
    bytes: usize,
    event_seq: u64,
    prepare_seq: u64,
}

struct QueuedCommand {
    command: BatcherCommand,
    permit: OwnedSemaphorePermit,
    bytes: usize,
}

pub(super) enum AdmissionOutcome {
    Accepted,
    ReplacedOldest(usize),
}

impl SharedQueue {
    fn close(&self) {
        let mut state = self.state.lock().expect("batch admission poisoned");
        state.accepting = false;
        self.slots.close();
        // Closing cannot require a queue slot or a suspended producer's permit.
        self.closing.send_replace(true);
        self.ready.notify_one();
        self.space.notify_waiters();
        self.prepare_progress.notify_waiters();
    }

    /// 关闭后清空已登记的准备任务；槽与 permit 随请求 Drop 归还。
    fn clear_prepare(&self) {
        let mut pending = self.prepare.lock().expect("batch prepare poisoned");
        pending.clear();
    }
}

impl Admission {
    /// 测试构造入口：只有容量/字节限额参与，其余按默认部署配置。
    #[cfg(test)]
    pub(super) fn for_test(
        queue_capacity: usize,
        max_event_bytes: usize,
        max_queue_bytes: usize,
    ) -> (Self, CommandReceiver, watch::Receiver<bool>) {
        let config = BatcherConfig {
            queue_capacity,
            max_event_bytes,
            max_queue_bytes,
            ..Default::default()
        };
        Self::with_limits(&config)
    }

    pub(super) fn with_limits(
        config: &BatcherConfig,
    ) -> (Self, CommandReceiver, watch::Receiver<bool>) {
        let (closing, receiver) = watch::channel(false);
        let shared = Arc::new(SharedQueue {
            state: Mutex::new(QueueState {
                accepting: true,
                commands: VecDeque::new(),
                bytes: 0,
                event_seq: 0,
                prepare_seq: 0,
            }),
            slots: Arc::new(Semaphore::new(config.queue_capacity)),
            prepare_slots: Arc::new(Semaphore::new(config.queue_capacity)),
            prepare: Mutex::new(VecDeque::new()),
            ready: Notify::new(),
            space: Notify::new(),
            prepare_progress: Arc::new(Notify::new()),
            max_event_bytes: config.max_event_bytes,
            max_queue_bytes: config.max_queue_bytes,
            closing,
        });
        (
            Self {
                shared: Arc::clone(&shared),
            },
            CommandReceiver { shared },
            receiver,
        )
    }

    pub(super) fn close(&self) {
        self.shared.close();
    }

    /// 冻结准入：先槽后额，同一临界区内分配 `prepare_seq` 并登记准备任务。
    ///
    /// 拒绝路径不留下半登记：槽不足返回 `QueueFull`，额度不足返回 `MemoryBudgetExceeded`，
    /// 关闭后返回 `ChannelClosed`；已取到的槽与 permit 由局部变量 Drop 归还。
    pub(super) fn try_freeze(
        &self,
        source: Arc<dyn SnapshotSource>,
        budget: &Arc<MemoryBudget>,
        allowance_bytes: usize,
    ) -> Result<SnapshotTicket, LangfuseError> {
        if self.shared.slots.is_closed() {
            return Err(LangfuseError::ChannelClosed);
        }
        let slot = match Arc::clone(&self.shared.prepare_slots).try_acquire_owned() {
            Ok(slot) => slot,
            Err(TryAcquireError::Closed) => return Err(LangfuseError::ChannelClosed),
            Err(TryAcquireError::NoPermits) => return Err(LangfuseError::QueueFull),
        };
        // retained 上界必须是无大分配遍历；未知量按错误暴露，不记 0。
        let retained = budget.try_reserve(source.retained_bytes()?)?;
        let allowance = budget.try_reserve(allowance_bytes)?;
        // 序号分配与入队在同一 `state` 临界区内完成：否则两个并发 freeze 可能按相反顺序
        // 入队，水位可见的前缀不再等于序号前缀（契约 §3.6 的 freeze 线性化点）。
        // 锁序固定为 `state` → `prepare`，其它路径不反向取锁。
        let cell = {
            let mut state = self.shared.state.lock().expect("batch admission poisoned");
            if !state.accepting {
                return Err(LangfuseError::ChannelClosed);
            }
            let seq = state
                .prepare_seq
                .checked_add(1)
                .ok_or(LangfuseError::QueueFull)?;
            state.prepare_seq = seq;
            let cell = Arc::new(SnapshotCell {
                budget: Arc::clone(budget),
                prepare_seq: seq,
                state: Mutex::new(SnapshotState::Preparing),
                cancelled: AtomicBool::new(false),
                changed: Notify::new(),
            });
            let mut pending = self.shared.prepare.lock().expect("batch prepare poisoned");
            pending.push_back(PrepareRequest {
                cell: Arc::clone(&cell),
                source,
                allowance,
                retained,
                _slot: slot,
            });
            cell
        };
        self.shared.prepare_progress.notify_waiters();
        Ok(SnapshotTicket { cell })
    }

    pub(super) fn try_add(
        &self,
        event: TelemetryEvent,
        policy: BackpressurePolicy,
    ) -> Result<AdmissionOutcome, LangfuseError> {
        if self.shared.slots.is_closed() {
            return Err(LangfuseError::ChannelClosed);
        }
        if policy != BackpressurePolicy::DropOldest && self.shared.slots.available_permits() == 0 {
            return Err(LangfuseError::QueueFull);
        }
        let bytes = queued_bytes(&event, self.shared.max_event_bytes);
        let mut state = self.shared.state.lock().expect("batch admission poisoned");
        if !state.accepting {
            return Err(LangfuseError::ChannelClosed);
        }
        let bytes = bytes?;
        let mut permit = match Arc::clone(&self.shared.slots).try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(TryAcquireError::Closed) => return Err(LangfuseError::ChannelClosed),
            Err(TryAcquireError::NoPermits) => None,
        };
        let eligible = state
            .commands
            .iter()
            .rposition(|queued| matches!(queued.command, BatcherCommand::Flush { .. }))
            .map_or(0, |index| index + 1);
        let mut removed = Vec::new();
        let needs_replacement =
            permit.is_none() || bytes > self.shared.max_queue_bytes.saturating_sub(state.bytes);
        if needs_replacement {
            let candidates = &state.commands;
            let reclaimable = candidates
                .iter()
                .skip(eligible)
                .map(|queued| queued.bytes)
                .sum::<usize>();
            if policy != BackpressurePolicy::DropOldest
                || eligible == candidates.len()
                || bytes
                    > self
                        .shared
                        .max_queue_bytes
                        .saturating_sub(state.bytes - reclaimable)
            {
                return Err(LangfuseError::QueueFull);
            }
            while permit.is_none()
                || bytes > self.shared.max_queue_bytes.saturating_sub(state.bytes)
            {
                let retired = state
                    .commands
                    .remove(eligible)
                    .expect("eligible command exists");
                state.bytes -= retired.bytes;
                if permit.is_none() {
                    permit = Some(retired.permit);
                }
                removed.push(retired.command);
            }
        }
        let seq = state
            .event_seq
            .checked_add(1)
            .ok_or(LangfuseError::QueueFull)?;
        state.event_seq = seq;
        state.bytes += bytes;
        state.commands.push_back(QueuedCommand {
            command: BatcherCommand::Add(QueuedEvent {
                seq,
                event,
                accepted_at_ms: accepted_at_ms(),
            }),
            permit: permit.expect("accepted event owns a queue slot"),
            bytes,
        });
        drop(state);
        self.shared.ready.notify_one();
        let count = removed.len();
        drop(removed);
        self.shared.space.notify_waiters();
        Ok(if count == 0 {
            AdmissionOutcome::Accepted
        } else {
            AdmissionOutcome::ReplacedOldest(count)
        })
    }

    pub(super) async fn send(&self, event: TelemetryEvent) -> Result<(), LangfuseError> {
        if self.shared.slots.is_closed() {
            return Err(LangfuseError::ChannelClosed);
        }
        let bytes = queued_bytes(&event, self.shared.max_event_bytes)?;
        let permit = Arc::clone(&self.shared.slots)
            .acquire_owned()
            .await
            .map_err(|_| LangfuseError::ChannelClosed)?;
        loop {
            let space = self.shared.space.notified();
            tokio::pin!(space);
            space.as_mut().enable();
            {
                let mut state = self.shared.state.lock().expect("batch admission poisoned");
                if !state.accepting {
                    return Err(LangfuseError::ChannelClosed);
                }
                if bytes <= self.shared.max_queue_bytes.saturating_sub(state.bytes) {
                    let seq = state
                        .event_seq
                        .checked_add(1)
                        .ok_or(LangfuseError::QueueFull)?;
                    state.event_seq = seq;
                    state.bytes += bytes;
                    state.commands.push_back(QueuedCommand {
                        command: BatcherCommand::Add(QueuedEvent {
                            seq,
                            event,
                            accepted_at_ms: accepted_at_ms(),
                        }),
                        permit,
                        bytes,
                    });
                    self.shared.ready.notify_one();
                    return Ok(());
                }
            }
            space.await;
        }
    }

    /// 入队一条 flush 命令；两个水位在同一个 admission 锁内原子捕获（freeze 线性化点）。
    ///
    /// 返回本次捕获的边界：调用方据此核对 ack 属于同一个屏障。未入队的等待者不建立前缀保护。
    pub(super) async fn flush_command(
        &self,
        reply: tokio::sync::oneshot::Sender<super::FlushAck>,
    ) -> Result<super::FlushBoundary, LangfuseError> {
        if self.shared.slots.is_closed() {
            return Err(LangfuseError::ChannelClosed);
        }
        let permit = Arc::clone(&self.shared.slots)
            .acquire_owned()
            .await
            .map_err(|_| LangfuseError::ChannelClosed)?;
        let mut state = self.shared.state.lock().expect("batch admission poisoned");
        if !state.accepting {
            return Err(LangfuseError::ChannelClosed);
        }
        let boundary = super::FlushBoundary {
            event_seq: state.event_seq,
            prepare_seq: state.prepare_seq,
        };
        state.commands.push_back(QueuedCommand {
            command: BatcherCommand::Flush { boundary, reply },
            permit,
            bytes: 0,
        });
        drop(state);
        self.shared.ready.notify_one();
        Ok(boundary)
    }
}

impl CommandReceiver {
    pub(super) fn close(&self) {
        self.shared.close();
    }

    /// 准备任务登记/完成的唤醒点：worker 空闲时由它唤醒，忙碌时由每次循环顶部的
    /// [`CommandReceiver::take_prepare_upto`] 重查兜底（`notify_waiters` 不保存给未来等待者的许可）。
    pub(super) fn prepare_progress(&self) -> Arc<Notify> {
        Arc::clone(&self.shared.prepare_progress)
    }

    /// 是否存在 `prepare_seq <= boundary` 的待执行登记（flush 前缀屏障必须先处理这些）。
    pub(super) fn pending_prepare_upto(&self, boundary: u64) -> bool {
        let pending = self.shared.prepare.lock().expect("batch prepare poisoned");
        pending
            .front()
            .is_some_and(|request| request.cell.prepare_seq <= boundary)
    }

    /// 取走最早、且 `prepare_seq <= boundary` 的登记；后缀登记留在队列里由普通调度接手。
    /// 前缀屏障只覆盖自己的水位，不让后缀准备占用本次屏障的调度槽（契约 §3.6）。
    pub(super) fn take_prepare_upto(&self, boundary: u64) -> Option<PrepareRequest> {
        let mut pending = self.shared.prepare.lock().expect("batch prepare poisoned");
        if !pending
            .front()
            .is_some_and(|request| request.cell.prepare_seq <= boundary)
        {
            return None;
        }
        pending.pop_front()
    }

    pub(super) fn try_recv(&mut self) -> Option<BatcherCommand> {
        let command = {
            let mut state = self.shared.state.lock().expect("batch admission poisoned");
            let queued = state.commands.pop_front()?;
            state.bytes -= queued.bytes;
            queued.command
        };
        self.shared.space.notify_waiters();
        Some(command)
    }

    pub(super) async fn recv(&mut self) -> Option<BatcherCommand> {
        loop {
            let notified = self.shared.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.shared.state.lock().expect("batch admission poisoned");
                if let Some(queued) = state.commands.pop_front() {
                    state.bytes -= queued.bytes;
                    // The slot is returned before processing/HTTP, just as recv
                    // released the original mpsc command queue slot.
                    let command = queued.command;
                    drop(queued.permit);
                    drop(state);
                    self.shared.space.notify_waiters();
                    return Some(command);
                }
                if !state.accepting {
                    return None;
                }
            }
            notified.await;
        }
    }
}

impl Drop for CommandReceiver {
    fn drop(&mut self) {
        // Match mpsc receiver destruction on worker panic/abort: close blocked
        // producers and drop queued flush acks so waiters observe the join error.
        self.shared.close();
        let mut state = self.shared.state.lock().expect("batch admission poisoned");
        state.commands.clear();
        state.bytes = 0;
        drop(state);
        self.shared.clear_prepare();
    }
}

/// 事件准入时刻（wall-clock 毫秒）；与 `QueuedEvent::accepted_at_ms` 的冻结类型一致。
///
/// `client.rs` 有同口径的私有 `wall_ms()`；本处只做时间戳，等该处暴露 crate 内访问器后再共用。
fn accepted_at_ms() -> i64 {
    use std::time::UNIX_EPOCH;
    peri_time::now_wall()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;

#[cfg(test)]
#[path = "byte_budget_test.rs"]
mod byte_budget_tests;
