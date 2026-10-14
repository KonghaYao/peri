//! session 级准入裁决：撤销键与**已提交 peer**。
//!
//! 设计 §3.1 第 5 步：准入裁决放 session 级 owner（短锁、持锁不 await），
//! 提交产出携已绑定 peer 的 [`DispatchCommit`]；提交前撤销胜出 ⇒ 上游**零调用**，
//! 提交后只允许调用该句柄（不得换投同名新 peer）。
//!
//! 类型隔离（设计 §2）：本模块**不暴露** `cancel()`——关闭 App / 连接只撤销
//! 调用资格，不取消已提交的执行。

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use peri_acp_types::interaction::AttachmentId;
use peri_acp_types::mcp_apps::NotExecutedCause;

use super::super::apps::McpAppRecord;
use super::super::client::{McpClientHandle, McpClientPool};

/// 撤销键：被撤销的**调用资格**（不是执行令牌）。
///
/// 撤销只影响尚未提交的调用；已提交的调用由 invocation owner 收尾（设计 §2）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RevocationKey {
    /// 单个 App record（`apps/close` / 会话关闭）。
    App(String),
    /// 一个连接附件（连接关闭 / detach）：其名下全部 App record 一起撤销。
    Attachment(AttachmentId),
    /// 整个会话（session closing）。
    Session,
}

/// 已撤销键的有界集合（FIFO 淘汰）。容量远超单会话可能的 App / 连接数；
/// 淘汰不会重新放行调用——被撤销的 record 状态本身仍是终态（见
/// [`McpAppRecord::is_attached`]）。
const MAX_REVOKED_KEYS: usize = 4096;

#[derive(Default)]
struct RevokedSet {
    keys: HashSet<RevocationKey>,
    order: VecDeque<RevocationKey>,
}

impl RevokedSet {
    fn insert(&mut self, key: RevocationKey) {
        if !self.keys.insert(key.clone()) {
            return;
        }
        self.order.push_back(key);
        while self.order.len() > MAX_REVOKED_KEYS {
            if let Some(oldest) = self.order.pop_front() {
                self.keys.remove(&oldest);
            }
        }
    }

    fn contains(&self, key: &RevocationKey) -> bool {
        self.keys.contains(key)
    }
}

/// 一次提交所绑定的 peer（提交复核的输入）。
pub struct PeerBinding {
    /// 连接池（复核服务端世代用）。
    pub pool: Arc<McpClientPool>,
    /// 提交时绑定的连接句柄（**只允许**调用它）。
    pub peer: Arc<McpClientHandle>,
    /// 提交时的服务端世代。
    pub server_generation: u64,
    /// 承载本次调用的 App record（Host 调用无 App record：撤销键落在 session 层）。
    pub record: Option<Arc<McpAppRecord>>,
}

/// session 级准入 owner：串行裁决 epoch / 撤销 / 换代，产出已绑定 peer 的提交。
pub struct SessionAdmissionOwner {
    runtime_epoch: u64,
    revoked: Mutex<RevokedSet>,
    seq: AtomicU64,
}

impl SessionAdmissionOwner {
    pub fn new(runtime_epoch: u64) -> Self {
        Self {
            runtime_epoch,
            revoked: Mutex::new(RevokedSet::default()),
            seq: AtomicU64::new(0),
        }
    }

    pub fn runtime_epoch(&self) -> u64 {
        self.runtime_epoch
    }

    /// 撤销一个键（幂等）。此后以该键提交或复核的调用都显式未执行。
    pub fn revoke(&self, key: RevocationKey) {
        self.revoked.lock().insert(key);
    }

    /// 该键是否已被撤销。
    pub fn is_revoked(&self, key: &RevocationKey) -> bool {
        self.revoked.lock().contains(key)
    }

    /// 串行裁决（短锁、无 await）：epoch / 撤销 / record 状态 / 世代全部通过后
    /// 产出携已绑定 peer 的提交。
    ///
    /// 任一不成立 ⇒ `Err(cause)`：调用方据此产出 `NotExecuted{cause}`，并且
    /// **零 wire 调用**。
    pub fn commit(
        self: &Arc<Self>,
        key: RevocationKey,
        epoch: u64,
        binding: PeerBinding,
    ) -> Result<DispatchCommit, NotExecutedCause> {
        let revoked = self.revoked.lock();
        if self.runtime_epoch != epoch || revoked.contains(&key) {
            return Err(NotExecutedCause::TargetRevoked);
        }
        if let Some(record) = binding.record.as_ref() {
            if !record.is_attached() || record.server_generation != binding.server_generation {
                return Err(NotExecutedCause::TargetRevoked);
            }
        }
        if binding.peer.peer.is_none()
            || binding.pool.handle_generation(&binding.peer) != binding.server_generation
        {
            return Err(NotExecutedCause::TargetRevoked);
        }
        drop(revoked);
        let sequence = self.seq.fetch_add(1, Ordering::Relaxed);
        Ok(DispatchCommit {
            key,
            pool: binding.pool,
            peer: binding.peer,
            server_generation: binding.server_generation,
            record: binding.record,
            owner: Arc::clone(self),
            sequence,
        })
    }
}

/// 提交产物：只能被 [`super::AdmittedPeerTool`] 消费一次。
///
/// 不实现 `Clone` / `Copy`：一次提交只对应一次 wire 调用资格。
pub struct DispatchCommit {
    key: RevocationKey,
    pool: Arc<McpClientPool>,
    peer: Arc<McpClientHandle>,
    server_generation: u64,
    record: Option<Arc<McpAppRecord>>,
    owner: Arc<SessionAdmissionOwner>,
    sequence: u64,
}

impl DispatchCommit {
    /// 提交序号（单调；日志与测试据此关联一次提交）。
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// 已绑定的连接句柄（**唯一**允许投递的 peer）。
    pub fn peer(&self) -> &Arc<McpClientHandle> {
        &self.peer
    }

    /// 提交后、wire 调用前的复核（同步，短锁）。
    ///
    /// 撤销 / record 失效 / 世代变化 / 连接断开 ⇒ `Err(cause)`：调用方必须
    /// **不发出** wire 调用。
    pub fn recheck(&self) -> Result<(), NotExecutedCause> {
        if self.owner.is_revoked(&self.key) {
            return Err(NotExecutedCause::TargetRevoked);
        }
        if let Some(record) = self.record.as_ref() {
            if !record.is_attached() || record.server_generation != self.server_generation {
                return Err(NotExecutedCause::TargetRevoked);
            }
        }
        if self.peer.peer.is_none()
            || self.pool.handle_generation(&self.peer) != self.server_generation
        {
            return Err(NotExecutedCause::TargetRevoked);
        }
        Ok(())
    }
}
