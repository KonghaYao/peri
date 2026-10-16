//! `MessageTranscript` 的持久化接线（前台端口）。
//!
//! 职责：绑定后端与预算、在持锁路径上同步预留额度、把操作投递到后台 writer
//! （见 [`persistence`]），并在 flush / shutdown / Drop 时排空队列；失败一律写成
//! sticky 失败（热态不再可信）。后台 writer 不持有 transcript guard。

use std::sync::Arc;

use anyhow::anyhow;

use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::MessageFlags;

use crate::messages::MessageId;
use crate::thread::ThreadId;

use super::persistence::{self, PersistenceBudget, Reservation};
use super::{
    MessageTranscript, PersistOp, TranscriptEntry, DEFAULT_PENDING_MAX_BYTES,
    DEFAULT_PENDING_MAX_ITEMS,
};

impl MessageTranscript {
    /// 绑定会话资源门面
    ///
    /// 绑定后 append / rewind / 投影变更自动异步写入门面（FIFO 单一 writer）。
    /// 积压有界性由 [`PersistenceBudget`] 保证：追加在持锁时同步预留，writer 在
    /// 效果确定后归还；预留失败即 sticky 失败（[`Self::persistence_failure`]），
    /// 调用方据此停止后续工作并重载会话。
    pub fn with_persistence(self, store: Arc<dyn SessionResources>, thread_id: ThreadId) -> Self {
        let budget = PersistenceBudget::new(DEFAULT_PENDING_MAX_ITEMS, DEFAULT_PENDING_MAX_BYTES);
        self.bind_persistence(store, thread_id, budget)
    }

    /// 绑定持久化后端并显式指定待持久化预算（测试与阈值测量入口）。
    pub fn with_persistence_budget(
        self,
        store: Arc<dyn SessionResources>,
        thread_id: ThreadId,
        budget: Arc<PersistenceBudget>,
    ) -> Self {
        self.bind_persistence(store, thread_id, budget)
    }

    fn bind_persistence(
        mut self,
        store: Arc<dyn SessionResources>,
        thread_id: ThreadId,
        budget: Arc<PersistenceBudget>,
    ) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<PersistOp>();
        self.persist_tx = Some(Arc::new(tx));
        self.thread_id = Some(thread_id.clone());
        self.session_resources = Some(store.clone());
        self.budget = Some(Arc::clone(&budget));

        let handle = tokio::spawn(persistence::run_writer(store, thread_id, budget, rx));
        self.persist_handle = Some(handle.abort_handle());

        self
    }

    /// 待持久化失败原因（sticky）：预算耗尽、writer 终态失败或写通道关闭。
    ///
    /// 非 `None` 表示热态已不可信：数据可能已进内存但未落盘，调用方必须停止后续
    /// 模型/工具工作并让会话走冷重载，不得把当前快照当作已保存。
    pub fn persistence_failure(&self) -> Option<String> {
        self.budget.as_ref().and_then(|budget| budget.failure())
    }

    /// 是否已进入 sticky 持久化失败。
    pub fn has_persistence_failure(&self) -> bool {
        self.persistence_failure().is_some()
    }

    /// 预算句柄（测试断言积压上界用）。
    pub fn persistence_budget(&self) -> Option<Arc<PersistenceBudget>> {
        self.budget.clone()
    }

    // ── 内部辅助 ────────────────────────────────────────────────────────────────

    /// 等待此前已排队的持久化操作完成。
    ///
    /// 同一 writer 按 FIFO 处理 barrier，因此收到确认时，所有此前操作都已调用 store。
    /// 首个持久化错误会保持为终态失败；后续 barrier 继续返回该错误，不消费或重试。
    pub async fn flush_persistence(&self) -> anyhow::Result<()> {
        let Some(tx) = self.persist_tx_handle() else {
            return Ok(());
        };
        Self::flush_via_tx(&tx).await
    }

    /// 同步取出持久化 writer 通道句柄（owned `Arc<Sender>`，Send）。
    ///
    /// 用途：调用方持有 `Arc<RwLock<MessageTranscript>>` 时，先在 guard 作用域内
    /// 同步提取句柄、释放 guard，再调用 [`flush_via_tx`] 异步等待——避免
    /// parking_lot guard 跨 await 存活（`!Send`，会令整个调用链 future 不满足
    /// `Send`，`tokio::spawn` 编译失败）。
    pub fn persist_tx_handle(&self) -> Option<Arc<tokio::sync::mpsc::UnboundedSender<PersistOp>>> {
        self.persist_tx.clone()
    }

    /// barrier 等待逻辑（基于 owned sender，Send 安全）
    pub async fn flush_via_tx(
        tx: &tokio::sync::mpsc::UnboundedSender<PersistOp>,
    ) -> anyhow::Result<()> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        tx.send(PersistOp::Barrier(ack_tx))
            .map_err(|_| anyhow!("transcript persistence writer channel closed"))?;
        ack_rx
            .await
            .map_err(|_| anyhow!("transcript persistence writer dropped barrier acknowledgement"))?
    }

    /// 预留一笔待持久化额度：持锁期间同步完成，不 await。
    ///
    /// 预算耗尽时**不发送**该操作并留下 sticky 失败——数据已进 canonical 内存但
    /// 没有落盘名额，调用方只能据 [`Self::persistence_failure`] 停止后续工作。
    pub(super) fn reserve_persistence(&self, reservation: Reservation) -> Option<Reservation> {
        let budget = self.budget.as_ref()?;
        match budget.try_reserve(reservation) {
            Ok(()) => Some(reservation),
            Err(reason) => {
                tracing::error!(
                    items = reservation.items,
                    bytes = reservation.bytes,
                    "transcript persistence backlog limit reached: {reason}"
                );
                None
            }
        }
    }

    /// 追加条目落盘：按 canonical payload 编码长度预留额度后投递。
    pub(super) fn persist_appended_entry(&self, entry: TranscriptEntry) {
        let payload = entry.into_payload();
        let Some(reserved) = self.reserve_persistence(Reservation::for_payload(&payload)) else {
            return;
        };
        self.send_persist(PersistOp::Append { payload, reserved });
    }

    /// 投影/flags 变更落盘：队列里只有 id 与标记，按条数预留。
    pub(super) fn persist_flags(&self, id: MessageId, flags: MessageFlags) {
        let Some(reserved) = self.reserve_persistence(Reservation::marker(1)) else {
            return;
        };
        self.send_persist(PersistOp::UpdateFlags {
            id,
            flags,
            reserved,
        });
    }

    /// 发送持久化操作到 writer task（额度已在调用点预留）
    pub(super) fn send_persist(&self, op: PersistOp) {
        if let Some(ref tx) = self.persist_tx {
            if let Err(e) = tx.send(op) {
                // 通道关闭 = 写入不会发生：与预算耗尽同样是 sticky 失败。
                if let Some(budget) = self.budget.as_ref() {
                    budget.mark_failed(&format!(
                        "transcript persistence writer channel closed: {e}"
                    ));
                }
                tracing::warn!("transcript persist send failed (channel closed): {e}");
            }
        }
    }

    /// 优雅关闭持久化 writer task：发送 `Shutdown` 信号，writer flush 剩余积压后自行退出。
    ///
    /// 不调用 `abort()`：abort 会立即取消任务，导致 `pending_appends` 和通道中未处理的
    /// 消息被直接丢弃（参照 `langfuse-client/src/batcher.rs` 的 Shutdown 模式）。
    /// Drop 是同步的无法 await，因此不等待 writer 完成：writer 持有 store 的独立 Arc
    /// （`with_persistence` 中 clone），detached 收尾安全。
    pub fn shutdown_persistence(&self) {
        if let Some(ref tx) = self.persist_tx {
            if let Err(e) = tx.send(PersistOp::Shutdown) {
                // writer 已退出（channel closed）时无需处理：退出前已 flush 剩余积压
                tracing::debug!("transcript persist shutdown send failed (channel closed): {e}");
            }
        }
    }
}

impl Drop for MessageTranscript {
    fn drop(&mut self) {
        self.shutdown_persistence();
    }
}
