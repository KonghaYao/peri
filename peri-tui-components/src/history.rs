//! 输入历史浏览游标。
//!
//! 只持有「当前浏览位置 + 进入历史模式时保存的草稿」；历史条目本身由宿主
//! 用一份 `VecDeque` 保存，组件不复制整栈。
//!
//! - [`HistoryCursor::up`]：向旧方向移动，首次进入时保存当前编辑文本。
//! - [`HistoryCursor::down`]：向新方向移动；越过最新时退出历史模式并交出草稿。
//! - [`HistoryCursor::reset`]：提交、切换会话或开始新的编辑动作后回到编辑态。
//!
//! 草稿用 `Option<String>` 区分「从未保存草稿」（`None`）与「显式空输入」
//! （`Some("")`）——后者回到底部时必须清空编辑器，不能把最旧历史项留在框里。

use std::collections::VecDeque;

/// 输入历史浏览游标。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryCursor {
    index: Option<usize>,
    draft: Option<String>,
}

/// [`HistoryCursor::down`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryDown {
    /// 移动到更新的历史项。
    Entry(String),
    /// 退出历史模式；`Some("")` 表示显式空草稿，`None` 表示未保存过草稿。
    Draft(Option<String>),
    /// 本来就不在历史模式，无需恢复。
    Idle,
}

impl HistoryCursor {
    /// 当前浏览位置；`None` 表示编辑态。
    #[must_use]
    pub fn index(&self) -> Option<usize> {
        self.index
    }

    /// 进入历史模式时保存的草稿；`Some("")` 是显式空输入。
    #[must_use]
    pub fn draft(&self) -> Option<&str> {
        self.draft.as_deref()
    }

    /// 向旧方向浏览一步，返回该位置的历史文本（只复制选中项）。
    ///
    /// 空历史直接返回 `None` 且不改变状态；已在最旧位置则维持并返回该位置。
    /// 首次进入历史模式时保存 `current_text`（包括空串）。
    pub fn up(&mut self, history: &VecDeque<String>, current_text: Option<&str>) -> Option<String> {
        if history.is_empty() {
            return None;
        }
        let new_idx = match self.index {
            None => {
                self.draft = current_text.map(str::to_string);
                history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.index = Some(new_idx);
        history.get(new_idx).cloned()
    }

    /// 向新方向浏览一步。
    ///
    /// 超过最新位置时退出历史模式并交出草稿（[`HistoryDown::Draft`]）；
    /// 不在历史模式时返回 [`HistoryDown::Idle`]。
    pub fn down(&mut self, history: &VecDeque<String>) -> HistoryDown {
        let Some(index) = self.index else {
            return HistoryDown::Idle;
        };
        let new_idx = index + 1;
        if new_idx >= history.len() {
            self.index = None;
            return HistoryDown::Draft(self.draft.take());
        }
        self.index = Some(new_idx);
        match history.get(new_idx) {
            Some(text) => HistoryDown::Entry(text.clone()),
            // 历史在建索引后被外部截短：保守退回编辑态。
            None => {
                self.index = None;
                HistoryDown::Idle
            }
        }
    }

    /// 清空浏览指针与草稿，回到编辑新文本状态。
    pub fn reset(&mut self) {
        self.index = None;
        self.draft = None;
    }
}

#[cfg(test)]
#[path = "history_test.rs"]
mod tests;
