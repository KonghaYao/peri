//! 面板操作的请求协调原语：作用域、取值票、完成门与完成词汇。
//!
//! 提炼自 Plugin 搜索、Plugin 操作与会话级守卫中同构的「作用域 + 代次 + 取消 +
//! 过期拒收」，落在方案 §8 指定的 `application/operations/`。
//!
//! 边界（刻意的非目标）：
//! - 只做请求协调：不持有已发送变更的本地任务（task 仍由各面板 `tokio::spawn`
//!   持有），不建操作历史，不引入全局串行锁。
//! - 不读 atoms、不依赖 `crate::kit`：作用域由调用方（面板/装配层）从当前投影
//!   取值后传入。
//! - 取消只表示「本视图不再等待」，不代表业务已撤销：结果未确认必须用
//!   [`OperationOutcome::OutcomeUnknown`] 表达，不能呈现为成功或已取消。
//!
//! 状态与词汇的分工：`Idle` / `Running` 由 [`CompletionGate::is_pending`] 推导，
//! 本模块只把终态（成功 / 失败 / 未知）词汇化。

mod gate;
mod outcome;
mod scope;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

pub(crate) use gate::{CompletionGate, OperationTicket};
pub(crate) use outcome::{OperationOutcome, OutcomeUnknownReason};
pub(crate) use scope::OperationScope;
