//! 原子终态结算的结果。

use crate::thread::AgentStatus;

/// 无结果终态写入的结果：本次是否真的写入，以及既有终态是什么。
///
/// `AlreadyTerminal` 是重复收尾的幂等 no-op，不表示结算失败；调用方不得再补写
/// 另一组终态/工作意图。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalSettlementOutcome {
    /// 本次调用把 child 从 `active` 写为终态，并写入对应的工作意图。
    Settled,
    /// child 已有终态；本次调用没有改写任何列。
    AlreadyTerminal(AgentStatus),
}
