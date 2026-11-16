//! 历史窗口桥接事件：`peri/history_committed` 与窗口页接纳。
//!
//! 更新窗口账目、展示正文并经统一事务释放段，不调用工具完成、Todo、审批、输入或
//! steer handler；也不修改 `phase`/`current_request_id`/`turn_generation`/
//! `last_successful_todos`。

#[cfg(test)]
#[path = "history_window_test.rs"]
mod tests;

#[cfg(test)]
use super::history_page;

mod page_admission;
pub(crate) use page_admission::*;

mod open_display_plan;
use self::open_display_plan as open_display;
#[cfg(test)]
#[cfg(test)]
use open_display::remove_displayed_ranges;

#[cfg(test)]
pub(crate) use history_page::ToolResultSource;
