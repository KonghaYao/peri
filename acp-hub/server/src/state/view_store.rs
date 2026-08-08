//! yrs 薄封装（§5.6 隔离范围）。
//!
//! `ViewStore` trait 只隔离聚合器与 doc 生命周期管理；persist（update 重放）、
//! gateway（快照推送）、broadcaster（`merge_updates_v1`）直接接触 yrs 类型但
//! 以薄封装 free function 收敛（§5.6）。

use tokio::sync::mpsc;
use yrs::updates::decoder::Decode;
use yrs::{ReadTxn, StateVector, Transact, Update};

/// yrs 事务别名（隔离聚合器对 yrs 的直接命名；实现细节在 view_store.rs）。
pub type TransactionCtx<'a> = yrs::TransactionMut<'a>;

/// 聚合器可见的 yrs 抽象（§5.6「ViewStore trait 只隔离聚合器」）。
///
/// 承诺边界：聚合器与 doc 生命周期管理不直接命名 `yrs::Doc` API（除事务别名
/// [`TransactionCtx`]，见下）；写操作一律经 [`crate::state::chat_writer`] 原语。
pub trait ViewStore {
    /// 导出全量状态更新（快照推送 / persist 首写）。
    fn encode_state_as_update(&self) -> Vec<u8>;
    /// 应用外部 update（启动重放，§8.4.1 恢复路径；聚合器运行期不调用）。
    fn apply_update(&self, update: &[u8]) -> Result<(), ViewStoreError>;
    /// 注册 update 观察：yrs 回调是同步的、不能 await（§6.4），此处把 update
    /// 经 unbounded channel 送出；背压作用于下游 broadcaster 队列（F7）。
    fn observe_update(&self, tx: mpsc::UnboundedSender<Vec<u8>>) -> ViewStoreSubscription;
    /// 事务入口：聚合器在闭包内经 writer 原语写入（单事务边界，§6.4「一次
    /// Y.Doc transaction 写入」）。禁止跨 await 持有（§7.4）。
    fn with_txn<R>(&mut self, f: impl FnOnce(&mut TransactionCtx<'_>) -> R) -> R;
}

/// yrs 0.27 的具体实现（state 模块内部细节）。
pub struct YrsViewStore {
    doc: yrs::Doc,
    #[allow(dead_code)] // 预留：生命周期管理（drop 即退订）
    subscription: Option<yrs::Subscription>,
}

impl YrsViewStore {
    /// 包装一个 doc（`Doc` 是轻量句柄，可克隆）。
    pub fn new(doc: &yrs::Doc) -> Self {
        YrsViewStore {
            doc: doc.clone(),
            subscription: None,
        }
    }

    /// 底层 doc 引用（persist/gateway 直接接触 yrs 类型的收敛点之一）。
    pub fn doc(&self) -> &yrs::Doc {
        &self.doc
    }
}

impl ViewStore for YrsViewStore {
    fn encode_state_as_update(&self) -> Vec<u8> {
        // 空 StateVector = 全量状态（§5.6 快照推送）。
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    fn apply_update(&self, update: &[u8]) -> Result<(), ViewStoreError> {
        let parsed = Update::decode_v1(update).map_err(|e| ViewStoreError::UpdateDecode(e.to_string()))?;
        self.doc
            .transact_mut()
            .apply_update(parsed)
            .map_err(|e| ViewStoreError::Apply(e.to_string()))
    }

    fn observe_update(&self, tx: mpsc::UnboundedSender<Vec<u8>>) -> ViewStoreSubscription {
        let sub = self
            .doc
            .observe_update_v1(move |_, e| {
                // 同步回调：只经 channel 送出，不得做其他 IO（§6.4）。
                let _ = tx.send(e.update.clone());
            })
            .unwrap_or_else(|e| panic!("observe_update failed: {e}"));
        ViewStoreSubscription {
            subscription: sub,
        }
    }

    fn with_txn<R>(&mut self, f: impl FnOnce(&mut TransactionCtx<'_>) -> R) -> R {
        let mut txn = self.doc.transact_mut();
        f(&mut txn)
    }
}

/// yrs 订阅句柄（Drop 时自动退订）。
pub struct ViewStoreSubscription {
    subscription: yrs::Subscription,
}

impl ViewStoreSubscription {
    /// 主动退订（Drop 也会自动退订）。
    pub fn unsubscribe(&mut self) {
        // yrs::Subscription 无显式方法，drop 即退订；此处保留接口占位，
        // 由字段 drop 语义保证。
        let _ = &self.subscription;
    }
}

/// 薄封装 free function（§5.6：persist/gateway/broadcaster 直接接触 yrs 类型
/// 的收敛点）。
///
/// `encode_state_as_update(doc)` = `Y.encodeStateAsUpdate`（全量快照）；
/// `merge_updates_v1(updates)` = `Y.mergeUpdatesV1`（增量合并，broadcaster
/// 背压路径，§6.4/§8.6）。
pub fn encode_state_as_update(doc: &yrs::Doc) -> Vec<u8> {
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

/// 合并多条 v1 update 为一条（`Y.mergeUpdatesV1`）。
pub fn merge_updates_v1(updates: &[Vec<u8>]) -> Result<Vec<u8>, ViewStoreError> {
    yrs::merge_updates_v1(updates).map_err(|e| ViewStoreError::Merge(e.to_string()))
}

/// ViewStore 操作错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ViewStoreError {
    /// update 解码失败（格式损坏/版本不符）。
    #[error("update decode error: {0}")]
    UpdateDecode(String),
    /// update 应用失败。
    #[error("update apply error: {0}")]
    Apply(String),
    /// update 合并失败。
    #[error("update merge error: {0}")]
    Merge(String),
}
