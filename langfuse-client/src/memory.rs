//! 部署级内存预算与工作区租约（契约 §3.4、§4）。
//!
//! 预算身份以 `Arc` 指针相等性判定：金额相同不算同一预算，不允许重新挂账洗掉来源身份。
//! 预算分两个不互借的分区：**retained**（source/event/payload，经 [`MemoryBudget::try_reserve`]）
//! 与**进度分区** P（准备/HTTP 工作区与固定账单，只经工作区租约取得）。
//!
//! `used` 包含已隔离的 P：retained 预约只扣 retained 分区，空闲进度额度不能被 source 抢占。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::client::ExportConfig;
use crate::config::BatcherConfig;
use crate::error::LangfuseError;

#[cfg(test)]
#[path = "memory_test.rs"]
mod tests;

#[path = "memory/memory_budget.rs"]
mod memory_budget;
pub(crate) use memory_budget::slot_capacity;
pub use memory_budget::MemoryBudget;
pub use memory_budget::MemoryPermit;
pub(crate) use memory_budget::WorkspaceLease;
pub(crate) use memory_budget::GZIP_BILLING_BYTES;
