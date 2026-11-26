//! 主时间线与 SubAgent 详情面板共用的视口策略（D）。
//!
//! 这里只保留纯算术与判定：给定内容高 / 视口高 / 旧上缘，算出新上缘。
//! 不读 atom、不做布局、不持有状态——两个 surface 各自持有状态与绘制，但
//! 「最大可滚、clamp、初始位置、吸底跟随」只有这一份实现。
//!
//! 阅读锚点重定位**不在这里**：主区用的是身份口径（`viewport_anchor` +
//! `SlotIndex`），面板尚未引入锚点补偿。两者形状不同，合并留给 `TimelineStore`
//! （指南 §5 C1），当前不预造第二套 relocate。

/// 最大可滚动上缘：内容高不足一屏时为 0。
///
/// 跟随态用 `usize::MAX` 作「贴底」哨兵，`clamp_top` 会把它收敛成该值。
#[must_use]
pub(crate) fn max_scroll(height: usize, viewport: usize) -> usize {
    height.saturating_sub(viewport)
}

/// 把上缘收敛到 `[0, max_scroll]`。
#[must_use]
pub(crate) fn clamp_top(top: usize, height: usize, viewport: usize) -> usize {
    top.min(max_scroll(height, viewport))
}

/// 打开/重建时的初始上缘：running 贴尾，completed 贴头。
#[must_use]
pub(crate) fn initial_top(running: bool, height: usize, viewport: usize) -> usize {
    if running {
        max_scroll(height, viewport)
    } else {
        0
    }
}

/// 吸底跟随：源变化且变高、原本已在尾部时贴新尾部；否则只做 clamp。
///
/// `previous_height == 0`（还没有内容基线）时不算「在尾部」，与面板/主区既有口径
/// 一致：首帧由 [`initial_top`] 决定位置，不在这里补。
#[must_use]
pub(crate) fn follow_top(
    top: usize,
    previous_height: usize,
    previous_viewport: usize,
    height: usize,
    viewport: usize,
    source_changed: bool,
) -> usize {
    let at_bottom = previous_height > 0 && top >= max_scroll(previous_height, previous_viewport);
    if source_changed && height > previous_height && at_bottom {
        return max_scroll(height, viewport);
    }
    clamp_top(top, height, viewport)
}
