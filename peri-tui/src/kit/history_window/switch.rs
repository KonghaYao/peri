//! 窗口身份切换前的只读快照。
//!
//! 调用方必须先取快照、协调 `committed`，最后才调用
//! [`HistoryWindow::begin_window`]；后者会清空 index/display/segments。

use std::cmp::max;

use super::{HistoryWindow, PersistentEntryKey, WindowIdentity};

/// 身份切换前的只读事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSwitchSnapshot {
    pub identity_changed: bool,
    pub prior_generation: u64,
    pub displayed_entry_count: usize,
    /// `committed` 坐标中的展示区间（升序、互不重叠）。
    pub displayed_slot_ranges: Vec<(usize, usize)>,
    /// 无 slot 的展示身份及其持久位置（adopt 条目）。身份切换后须重新登记。
    pub slotless_entries: Vec<(PersistentEntryKey, Option<u64>)>,
    /// 已展示自有条目的最大持久位置；继承条目没有 position，不参与。
    pub max_displayed_position: Option<u64>,
}

impl HistoryWindow {
    /// 读取身份切换前的事实，不修改任何状态。
    pub fn switch_snapshot(&self, next: &WindowIdentity) -> WindowSwitchSnapshot {
        let segment_max = self
            .segments()
            .values()
            .flat_map(|segment| segment.entries.iter())
            .filter(|entry| self.is_entry_displayed(&entry.key))
            .filter_map(|entry| entry.position)
            .max();
        // adopt 条目没有 segment，但持久位置仍是 live 尾部的水位事实。
        let max_displayed_position = max(segment_max, self.display.max_position());
        WindowSwitchSnapshot {
            identity_changed: self.identity() != Some(next),
            prior_generation: self.generation(),
            displayed_entry_count: self.displayed_entry_count(),
            displayed_slot_ranges: self.display.slot_ranges(),
            slotless_entries: self.display.slotless_entries(),
            max_displayed_position,
        }
    }
}
