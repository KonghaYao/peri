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
    /// `session:{chat_id}`（§5.4，低频会话状态；对齐 Chat/Session 双 Doc）。
    pub session: yrs::Doc,
    /// 聚合器流状态（不进 yrs：可丢弃镜像不承载校准事实，§8.1 原则 5）。
    pub stream: StreamState,
}

impl DocPair {
    /// 打开 Chat Doc 写事务（chat → control 事务顺序的第一步；禁止跨 await
    /// 持有，§7.4）。
    pub fn chat_txn(&mut self) -> TransactionCtx<'_> {
        self.chat.transact_mut()
    }

    /// 打开 Session Doc 写事务（须在 chat 事务 drop 后调用，§6.4 固定顺序）。
    pub fn session_txn(&mut self) -> TransactionCtx<'_> {
        self.session.transact_mut()
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
    /// `session/load` 回放模式（§8.5 显式重建）：`replay_active` = 回放中
    /// （BeginLoadReplay → EndLoadReplay）；`replay_turn` = 当前回放 turn
    /// 归位 id（历史 chunk 无 turnId，按回放序归位，§7.2 宿主驱动模型的
    /// 回放例外）；`replay_turns` = 全部回放 turn（EndLoadReplay 逐个终态
    /// 化——历史 agent 消息无终态事件）。
    ///
    /// 复位语义（原 `reset`，已删除——无调用点）：不可校准缺口只能经
    /// `session/load` 显式重建消除（BeginLoadReplay 清除 uncalibratable /
    /// gap_count / 置 gap_dirty，见 §8.5）；**epoch/last_seq 水位必须保持**
    /// ——instance 侧 per-chat seq 单调递增、load 不重置（进程重建才重置
    /// 并伴 epoch+1），回放帧 seq 延续旧流，重置水位会把回放帧误判为
    /// SeqOutOfOrder。
    pub replay_active: bool,
    /// 回放中最近一条 user 消息建立的归位 turn id。
    pub replay_turn: Option<String>,
    /// 本次回放建立的全部 turn id（按序；EndLoadReplay 消费）。
    pub replay_turns: Vec<String>,
}
