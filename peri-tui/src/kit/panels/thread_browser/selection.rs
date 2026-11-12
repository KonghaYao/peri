//! Thread Browser 的选中行与视口落位。
//!
//! 列表由后台快照周期性重排：选中项以 thread 身份为载体，行号只在当前帧有效。
//! 本模块把身份解析结果与滚动视口对齐，保证高亮可见、动作目标仍是列表成员。

use crate::kit::atoms::ThreadSummary;
use ratatui_kit::components::scroll_view::ScrollViewState;
use ratatui_kit::ratatui::layout::Position;

/// 刷新帧的选中行落位：身份解析换行或视口尺寸变化后，把选中行带回视口。
///
/// 后台快照每 2 秒重排列表，只挪光标不挪视口时选中行会停在视口之外——
/// 高亮看不见，Enter 却仍加载它。用户手动滚动（未换行、视口未变）不受影响。
pub(super) fn sync_refreshed_selection(
    state: &mut ScrollViewState,
    selected_id: Option<&str>,
    threads: &[ThreadSummary],
    cursor: usize,
    previous_viewport: usize,
    viewport: usize,
) -> usize {
    let selected = super::resolve_selection(selected_id, threads, cursor);
    if selected != cursor || previous_viewport != viewport {
        keep_selection_visible(state, selected, threads.len(), viewport);
    }
    selected
}

/// 动作目标：只接受当前列表里真实存在的身份（Enter 加载与 `a` 归档共用）。
///
/// 空刷新帧会保留上一帧的身份（见 [`super::adopt_selection`]），此时面板没有
/// 可指的行；对不可见身份发起加载或归档，用户操作的是自己看不见的会话。
pub(super) fn visible_target(
    selected_id: Option<&str>,
    threads: &[ThreadSummary],
) -> Option<String> {
    selected_id
        .filter(|id| threads.iter().any(|thread| thread.id == *id))
        .map(str::to_owned)
}

pub(super) fn keep_selection_visible(
    state: &mut ScrollViewState,
    selected: usize,
    count: usize,
    viewport: usize,
) {
    if count == 0 || viewport == 0 {
        return;
    }
    let max_start = count.saturating_sub(viewport);
    let mut start = state.offset().y as usize;
    if selected < start {
        start = selected;
    } else if selected >= start.saturating_add(viewport) {
        start = selected.saturating_add(1).saturating_sub(viewport);
    }
    state.set_offset(Position::new(0, start.min(max_start) as u16));
}
