//! TUI 历史分页：浏览状态、页响应解码、区间合并与触发（REQ-C01、验收 A11/A20/A23/A24）。
//!
//! ACP 侧的契约（见 `peri-acp/src/host/history_page.rs` 模块文档）：
//!
//! - 入口仍是 `session/load`：带 `cursor` + `limit` = 历史页，响应形状
//!   `{sessionId, snapshotToken, nextCursor, items[{entryId, position, oversize,
//!   subItems[{position, update}]}]}`；初始 load 的响应同样带 `nextCursor` /
//!   `snapshotToken`（仅协商过 `peri.historyPagination` 的连接才有）。
//! - 只在**同一个 `snapshotToken`** 内可以比较 `position`：它是存储序键（不透明整数），
//!   客户端不得解释，只用于排序与区间合并。继承区条目没有 `position`，先于全部自有条目。
//! - `entryId` 是页与首屏共用的唯一去重身份（`{会话}:{条目}`）。
//!
//! 本模块只做三件事，全部与实时状态无关（REQ-C01-6）：
//!
//! 1. **浏览状态**：`snapshotToken` / `nextCursor` / 在途请求。初始 load 响应与页响应
//!    是仅有的两个写入来源；触发侧只在「相同 cursor + 相同 limit 已在途」时合并请求。
//! 2. **区间合并**：按 `entryId` 去重、按 `position` 插入到正确区间（[`merge_page`]）。
//!    整页都早于当前最早条目时表现为插到自有条目区最前（继承前缀仍在更前）。
//! 3. **触发判定**：滚动接近向前边界时请求一页（[`request_older_page_if_due`]）。翻页
//!    loading 与执行状态分离——它不碰 `ACP_STATE.is_loading`，也不建立/结束 turn。
//!
//! 明确不做（REQ-C03）：不建前端持久化缓存、不做全历史虚拟化、不解析 `position` 的语义。

pub mod window;

#[cfg(test)]
#[path = "history_paging_test.rs"]
pub(crate) mod tests;

#[cfg(test)]
use crate::kit::atoms;
#[cfg(test)]
use crate::kit::history_window::HistoryWindow;

mod paging;
pub use paging::*;

pub(crate) use window::*;
