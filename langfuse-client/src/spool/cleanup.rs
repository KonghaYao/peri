//! 暂存对象清理注册表（契约 §3.2）：slot 在 create/恢复 claim 之前分配，由唯一 worker 持有强引用，
//! 句柄只持注册、不反向持 worker。删除协议：清理 owner 始终拥有同一个 [`ObjectClaim`]，delete 成功
//! 才 `disarm_after_delete` 回收注册/slot/已写配额，失败把 claim 归回原 slot 重试；Drop 不做 IO、
//! 不分配、不 spawn。只在同步临界区持锁、绝不跨 await，唯一阻塞操作是 `spawn_blocking` 的 delete；
//! 清理路径从 `Drop` 进入，故取锁容忍毒化（`into_inner`），回收不能被另一次 panic 二次带崩。
//!
//! dead_code 归因（无消费者，保留契约语义）：`CleanupSlot::seq` 是 §3.2 的「关联序号」（只写不读）、
//! `SlotState::Disarmed::handed_off` 区分移交来源（写入不读，计数另由 `disarmed_after_handoff` 记录）、
//! `CleanupRegistry::changed` 是尚无 awaiter 的 worker 唤醒点（`drain` 由 worker 周期触发）。
//! `CleanupRegistration::disarm_after_handoff` 已接线：生产移交分支在
//! `batcher/recovery.rs::release_hand_off` 调用，不再是死代码。细节与判断见实施报告 §7。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use tokio::sync::Notify;

use crate::memory::MemoryBudget;

use super::types::{ObjectClaim, ObjectId, SpoolStore};
use super::{SpoolError, SpoolResult};

/// 已分配清理槽的注册凭证；每个对象、临时文件与恢复产物各占一槽。
pub struct CleanupRegistration {
    pub(crate) slot: Arc<CleanupSlot>,
}

/// 固定容量槽表、[`tokio::sync::Notify`] 与预算身份；由唯一 worker 独占持有。
pub(crate) struct CleanupRegistry {
    shared: Arc<RegistryShared>,
}

struct RegistryShared {
    store: Arc<dyn SpoolStore>,
    budget: Arc<MemoryBudget>,
    capacity: usize,
    table: Mutex<Vec<Arc<CleanupSlot>>>, // 未 disarm 的槽；移除即归还容量
    changed: Notify,                     // 登记/回收的唤醒点，worker 用它代替轮询
    seq: AtomicU64,
    disarmed_after_delete: AtomicU64,
    disarmed_after_handoff: AtomicU64,
}

/// 单个槽：store/id/quota/claim、状态与关联序号；同代登记只能清理/释放一次。
pub(crate) struct CleanupSlot {
    registry: Weak<RegistryShared>,
    store: Arc<dyn SpoolStore>,
    budget: Arc<MemoryBudget>,
    /// 归因见文件头：§3.2 关联序号，只写不读（无读取入口）。
    #[allow(dead_code)]
    seq: u64,
    quota_bytes: AtomicU64, // create 登记的配额；publish 可用同 id 重登记为实际落盘长度
    state: Mutex<SlotState>,
}

/// 槽的清理归属状态；`Pending` 是唯一允许 blocking delete 的状态。
enum SlotState {
    Registered,
    Active {
        id: ObjectId,
    },
    /// 待清理：claim 已归槽（空则用 `SpoolStore::delete` 幂等删除）；`borrowed` 表示借用中。
    Pending {
        id: ObjectId,
        claims: Vec<Box<dyn ObjectClaim>>,
        borrowed: bool,
    },
    /// 已回收（`handed_off` 区分正常移交给下一 owner）；同代登记不得再清理或释放。
    Disarmed {
        #[allow(dead_code)] // 归因见文件头
        handed_off: bool,
    },
}

impl SlotState {
    /// 已绑定对象的 id；`Registered`/`Disarmed` 没有绑定。
    fn bound_id(&self) -> Option<ObjectId> {
        match self {
            SlotState::Active { id } | SlotState::Pending { id, .. } => Some(*id),
            SlotState::Registered | SlotState::Disarmed { .. } => None,
        }
    }
}

/// blocking job 借用 claim 的保险：即使 `ObjectClaim::delete` panic 也把 claim 归回槽。
struct BorrowGuard<'a> {
    slot: &'a CleanupSlot,
    id: ObjectId,
    claims: Vec<Box<dyn ObjectClaim>>,
}

impl BorrowGuard<'_> {
    /// 逐个删除（正常最多一个）。失败立即停止，未删成功的 claim 留在 guard 中归还。
    fn delete_all(&mut self) -> SpoolResult<()> {
        if self.claims.is_empty() {
            return self.slot.store.delete(self.id);
        }
        while let Some(mut claim) = self.claims.pop() {
            if let Err(error) = claim.delete() {
                self.claims.push(claim);
                return Err(error);
            }
        }
        Ok(())
    }
}

impl Drop for BorrowGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.slot.lock_state();
        if let SlotState::Pending {
            claims, borrowed, ..
        } = &mut *state
        {
            *borrowed = false;
            claims.append(&mut self.claims);
        }
    }
}

impl CleanupRegistry {
    /// 绑定唯一 store 与部署预算身份；`capacity` 见 [`slot_capacity`]。
    pub(crate) fn new(store: Arc<dyn SpoolStore>, budget: Arc<MemoryBudget>, slots: usize) -> Self {
        Self {
            shared: Arc::new(RegistryShared {
                store,
                budget,
                capacity: slots,
                table: Mutex::new(Vec::new()),
                changed: Notify::new(),
                seq: AtomicU64::new(0),
                disarmed_after_delete: AtomicU64::new(0),
                disarmed_after_handoff: AtomicU64::new(0),
            }),
        }
    }

    /// 同步分配一槽并登记；容量耗尽返回 [`SpoolError::Busy`]，不做 IO。
    pub(crate) fn try_register(&self) -> SpoolResult<CleanupRegistration> {
        let mut table = lock(&self.shared.table);
        if table.len() >= self.shared.capacity {
            return Err(SpoolError::Busy);
        }
        let slot = Arc::new(CleanupSlot {
            registry: Arc::downgrade(&self.shared),
            store: self.shared.store.clone(),
            budget: self.shared.budget.clone(),
            seq: self.shared.seq.fetch_add(1, Ordering::Relaxed),
            quota_bytes: AtomicU64::new(0),
            state: Mutex::new(SlotState::Registered),
        });
        table.push(Arc::clone(&slot));
        Ok(CleanupRegistration { slot })
    }

    /// 只由 worker 执行有界 blocking delete/abort，确认删除后回收 slot 与已写配额；单个删除失败不
    /// 阻塞其他待清理对象——状态与 claim 全保留、首个错误在全部尝试后返回，下一轮可重试。
    pub(crate) async fn drain(&mut self) -> SpoolResult<()> {
        let slots = self.pending_slots();
        if slots.is_empty() {
            return Ok(());
        }
        // 一次有界 blocking job 处理本轮全部待清理槽；panic 时守卫已把 claim 归槽。
        let job = tokio::task::spawn_blocking(move || {
            let mut first_error = None;
            for slot in slots {
                if let Err(error) = slot.delete_pending() {
                    first_error = first_error.or(Some(error));
                }
            }
            first_error
        });
        match job.await {
            Ok(Some(error)) => Err(error),
            Ok(None) => Ok(()),
            Err(_join) => Err(SpoolError::Io {
                operation: "cleanup delete",
                source: std::io::Error::other("cleanup delete job panicked"),
            }),
        }
    }

    /// 唤醒点：待清理登记与回收都会通知，worker 用它代替轮询。
    #[allow(dead_code)] // 归因见文件头：尚无 awaiter
    pub(crate) fn changed(&self) -> &Notify {
        &self.shared.changed
    }

    /// `(已确认删除回收数, 正常移交回收数)`；两者分离计数，同一登记只进其中之一。仅测试断言读取。
    #[cfg(test)]
    pub(crate) fn disarmed(&self) -> (u64, u64) {
        (
            self.shared.disarmed_after_delete.load(Ordering::Relaxed),
            self.shared.disarmed_after_handoff.load(Ordering::Relaxed),
        )
    }

    /// 待清理槽的 `(个数, 登记配额字节)` 快照。
    pub(crate) fn pending(&self) -> (usize, u64) {
        let slots = self.pending_slots();
        let bytes: u64 = slots.iter().map(|slot| slot.quota_bytes()).sum();
        (slots.len(), bytes)
    }

    fn pending_slots(&self) -> Vec<Arc<CleanupSlot>> {
        lock(&self.shared.table)
            .iter()
            .filter(|slot| slot.is_pending())
            .cloned()
            .collect()
    }
}

impl RegistryShared {
    /// 回收一槽：移除表项即归还容量；`outcome=Some(handed_off)` 计入对应回收计数，`None` 表示
    /// 从未绑定对象的归还（没有产物可删、不计数）；同代登记只生效一次。
    fn detach(&self, slot: &CleanupSlot, outcome: Option<bool>) {
        lock(&self.table).retain(|entry| !std::ptr::eq(&**entry, slot));
        if let Some(handed_off) = outcome {
            let counter = if handed_off {
                &self.disarmed_after_handoff
            } else {
                &self.disarmed_after_delete
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        self.changed.notify_waiters();
    }
}

impl CleanupSlot {
    /// `create`/恢复在槽内登记 store/id/quota 后才执行第一笔写入；同一 id 可重登记配额。
    pub(crate) fn bind(&self, id: ObjectId, quota_bytes: u64) -> SpoolResult<()> {
        {
            let mut state = self.lock_state();
            match &mut *state {
                SlotState::Registered => *state = SlotState::Active { id },
                SlotState::Active { id: bound } if *bound == id => {}
                _ => return Err(SpoolError::InvalidMetadata),
            }
        }
        self.quota_bytes.store(quota_bytes, Ordering::Relaxed);
        Ok(())
    }

    /// 清理 owner 借用同一个 claim 做幂等删除；成功后回收本槽与已写配额。
    fn delete_pending(&self) -> SpoolResult<()> {
        let (id, claims) = {
            let mut state = self.lock_state();
            match &mut *state {
                SlotState::Disarmed { .. } => return Ok(()),
                SlotState::Pending {
                    id,
                    claims,
                    borrowed,
                } if !*borrowed => {
                    *borrowed = true;
                    (*id, std::mem::take(claims))
                }
                _ => return Err(SpoolError::Busy),
            }
        };
        let mut guard = BorrowGuard {
            slot: self,
            id,
            claims,
        };
        match guard.delete_all() {
            Ok(()) => {
                // 删除已确认：先释放交付句柄（OS 锁），再回收槽与已写配额。
                guard.claims.clear();
                self.disarm(Some(false));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// 句柄销毁时把 claim 移回原槽并置 CleanupPending；不能只释放 OS 锁造成所有权真空。
    fn abandon(&self, claim: Option<Box<dyn ObjectClaim>>) {
        let claim_id = claim.as_ref().map(|claim| claim.meta().id);
        let mut unbound = false;
        {
            let mut state = self.lock_state();
            let target = state.bound_id().or(claim_id);
            match &mut *state {
                SlotState::Pending { claims, .. } => {
                    // 借用进行中的迟到句柄也留在槽内，由 blocking job 下一次通过清理。
                    if let Some(claim) = claim.filter(|claim| {
                        !claims.iter().any(|held| held.meta().id == claim.meta().id)
                    }) {
                        claims.push(claim);
                    }
                }
                SlotState::Disarmed { .. } => {}
                _ => match target {
                    Some(id) => {
                        *state = SlotState::Pending {
                            id,
                            claims: claim.into_iter().collect(),
                            borrowed: false,
                        }
                    }
                    // 尚未绑定对象、也没有产物：没有可清理的东西，直接归还槽位。
                    None => {
                        *state = SlotState::Disarmed { handed_off: false };
                        unbound = true;
                    }
                },
            }
        }
        if unbound {
            self.disarm(None);
        } else if let Some(shared) = self.registry.upgrade() {
            shared.changed.notify_waiters();
        }
    }

    /// 回收登记：`Some(handed_off)` 表示删除成功/正常移交，`None` 只归还从未绑定对象的槽；恰好
    /// 生效一次——重复调用与已绑定对象上的 `None` 都是幂等无操作（claim 不会丢）。
    fn disarm(&self, outcome: Option<bool>) {
        {
            let mut state = self.lock_state();
            let closed = matches!(&*state, SlotState::Disarmed { .. });
            if closed || (outcome.is_none() && state.bound_id().is_some()) {
                return;
            }
            *state = SlotState::Disarmed {
                handed_off: outcome == Some(true),
            };
        }
        if let Some(shared) = self.registry.upgrade() {
            shared.detach(self, outcome);
        }
    }

    /// 已绑定对象的 id；尚未 create/恢复时为 `None`。
    pub(crate) fn id(&self) -> Option<ObjectId> {
        self.lock_state().bound_id()
    }

    /// 槽所属的 store；`SpoolObject::from_claim` 用它校验同 store。
    pub(crate) fn store(&self) -> &Arc<dyn SpoolStore> {
        &self.store
    }

    /// 槽所属的部署预算身份；与各 permit 的 `Arc` 指针相等性比较。
    pub(crate) fn budget(&self) -> &Arc<MemoryBudget> {
        &self.budget
    }

    pub(crate) fn quota_bytes(&self) -> u64 {
        self.quota_bytes.load(Ordering::Relaxed)
    }

    fn is_pending(&self) -> bool {
        matches!(&*self.lock_state(), SlotState::Pending { .. })
    }

    fn lock_state(&self) -> MutexGuard<'_, SlotState> {
        lock(&self.state)
    }
}

impl CleanupRegistration {
    /// 句柄销毁时把 claim 移回原槽并置 CleanupPending；不能只释放 OS 锁造成所有权真空。
    pub(crate) fn abandon(&mut self, claim: Option<Box<dyn ObjectClaim>>) {
        self.slot.abandon(claim);
    }

    /// 确认删除完成后回收注册、slot 与已写配额；恰好调用一次。
    pub(crate) fn disarm_after_delete(self) {
        self.slot.disarm(Some(false));
    }

    /// shutdown 正常移交后调用：保留磁盘配额，只释放本地槽；Drop 不得隐式当作移交。
    pub(crate) fn disarm_after_handoff(self) {
        self.slot.disarm(Some(true));
    }
}

impl Drop for CleanupRegistration {
    /// 只归还「登记后从未绑定对象」的槽；已绑定对象的槽由原 owner 的 `abandon` 归槽后交 worker。
    fn drop(&mut self) {
        self.slot.disarm(None);
    }
}

/// 容忍毒化的取锁：Drop/清理路径不得因另一次 panic 二次 panic。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[path = "cleanup_test.rs"]
mod tests;
