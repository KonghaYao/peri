//! 每 chat 的双 Doc 组合 + 流状态（§5.2 / §8.5）。

use yrs::Transact;

use crate::state::view_store::TransactionCtx;

/// 每 chat 的双 Doc 组合 + 流状态（§5.2 / §8.5）。
///
/// 由 [`crate::state::factory::Factory`] 创建；只允许被该 chat 的单写者
/// writer task 独占（§7.4）。`&mut DocPair` 是聚合器纯函数
/// [`crate::state::aggregator::Aggregator::apply`] 的载体（§12 测试前提：内存
/// Y.Doc）。
#[derive(Debug)]
pub struct DocPair {
    /// `chat:{chat_id}`（§5.3，高频内容流）。
    pub chat: yrs::Doc,
    /// `control:{chat_id}`（§5.4，低频控制状态）。
    pub control: yrs::Doc,
    /// 聚合器流状态（不进 yrs：可丢弃镜像不承载校准事实，§8.1 原则 5）。
    pub stream: StreamState,
}

impl DocPair {
    /// 打开 Chat Doc 写事务（chat → control 事务顺序的第一步；禁止跨 await
    /// 持有，§7.4）。
    pub fn chat_txn(&mut self) -> TransactionCtx<'_> {
        self.chat.transact_mut()
    }

    /// 打开 Control Doc 写事务（须在 chat 事务 drop 后调用，§6.4 固定顺序）。
    pub fn control_txn(&mut self) -> TransactionCtx<'_> {
        self.control.transact_mut()
    }
}

/// 聚合器流状态：gap 计算与 interrupted 校准的重放序水位（§8.5 / §6.3）。
///
/// 启动时从 persist 的 `(epoch, last_seq)` 水位（F6）恢复；运行期内存维护，
/// 与 update 日志落盘同步更新（随提交 flush 交给 persist）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamState {
    /// 当前流纪元（§4.5.1）；与 instance/event 帧 epoch 不一致 → 帧丢弃并计数。
    pub epoch: u64,
    /// 已应用的最大 seq（同 epoch 单调；校准与 gap 判定依据）。
    pub last_seq: u64,
    /// 累计缺口帧数（seq 跳变增量；追平后清零）。
    pub gap_count: u64,
    /// epoch 变化/缓冲丢失触发 → 不可校准缺口（§8.5 uncalibratable）。
    pub uncalibratable: bool,
    /// 待上报的 gap 变化（上次上报后是否有 gap_count/uncalibratable 变化）。
    pub gap_dirty: bool,
}

impl StreamState {
    /// 复位（`session/load` 显式重建路径，F7 命令调用；§8.5 不可校准只能经此
    /// 消除）。
    pub fn reset(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.last_seq = 0;
        self.gap_count = 0;
        self.uncalibratable = false;
        self.gap_dirty = false;
    }
}
