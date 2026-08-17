//! 滚动节流 + 鼠标事件处理 + 吸底自动跟随。

use ratatui_kit::prelude::*;

mod auto_follow;
mod event;
mod geometry;
mod gesture;
mod throttle;

use self::auto_follow::update_follow_on_scroll;
#[cfg(test)]
use self::auto_follow::{anchor_scroll_target, should_follow_after_user_scroll};
pub(super) use self::auto_follow::{new_output_indicator_active, run_auto_follow};
pub(super) use self::event::handle_event;
#[cfg(test)]
use self::geometry::round_divide;
pub(super) use self::geometry::{ScrollbarDragState, is_scrollbar_column};
use self::geometry::{compute_thumb_geometry, position_to_scroll_y, thumb_start_to_position};
pub(super) use self::gesture::{DragAction, drag_step, freeze_down, settle_up};
#[cfg(test)]
use self::gesture::{entry_click_target, is_click};
use self::throttle::apply_scroll;
pub(super) use self::throttle::{DragThrottle, SCROLL_PADDING, flush_scroll_if_due};
pub(crate) use self::throttle::{SCROLL_LINES, ScrollThrottle, scroll_frame_ms};
#[cfg(test)]
use self::throttle::{apply_delta_to_offset, is_reverse_direction};
#[cfg(test)]
use crate::kit::message_area::props::ScrollbarFields;
#[cfg(test)]
use crate::kit::message_area::selection::WrappedLineInfo;
#[cfg(test)]
use ratatui_kit::ratatui::layout::Rect;

// ── 滚动状态 ──────────────────────────────────────────────────────────────

/// 消息区滚动状态——替代 ratatui-kit 的 `ScrollViewState`。
///
/// [Why] `ScrollViewState.offset` 是 ratatui `Position`（u16），`total_visual_rows`
/// 超过 65535 视觉行（100 列终端约 650 万字符，如长代码文件输出/大 diff 累积）时
/// 滚动上限被截断，真实底部（footer/spinner）不可达、scrollbar thumb 到底但内容没到底。
/// 自持 `usize` 偏移彻底解除上限。
///
/// [Why bottom] `scroll_to_bottom()` 设置最大偏移，渲染每帧 clamp 到当帧的
/// `max_scroll`——与旧 `ScrollViewState::scroll_to_bottom`（size 为 None 时设
/// `u16::MAX`）行为一致，但 usize 下无上限。
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct ScrollPos {
    offset_y: usize,
}

impl ScrollPos {
    pub(super) fn offset(&self) -> usize {
        self.offset_y
    }

    pub(super) fn set_offset(&mut self, y: usize) {
        self.offset_y = y;
    }

    pub(super) fn scroll_up(&mut self) {
        self.offset_y = self.offset_y.saturating_sub(1);
    }

    pub(super) fn scroll_down(&mut self) {
        self.offset_y = self.offset_y.saturating_add(1);
    }

    pub(super) fn scroll_to_top(&mut self) {
        self.offset_y = 0;
    }

    /// 滚动到底——偏移设为最大，渲染 clamp 到当帧 `max_scroll`。
    pub(super) fn scroll_to_bottom(&mut self) {
        self.offset_y = usize::MAX;
    }
}

// ── 左键手势状态机（Pending → Armed → settled）────────────────────────

/// 消息区内一次左键手势的中间状态（取代 `selection_down_pos` 的语义）。
///
/// 状态表达：`None` = Idle；`Some` = Pending（Down 已记录、未升级为拖拽）；
/// Drag 超容差升级后置 `None`，由 `text_sel.dragging == true` 表达 Armed。
///
/// [Why 冻结] Down 时一次性换算并冻结内容坐标与 entry 命中——Up 结算只
/// 消费冻结结果，不再二次换算（滚动偏移/网格前缀的坐标正确性由 Down 保证）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GesturePending {
    /// 按下点屏幕坐标 `(column, row)`——唯一参与判定的坐标（`is_click` 比较）。
    pub(super) screen: (u16, u16),
    /// 按下点内容坐标（视觉行/列）——Down 时换算冻结，升级时作 `start_drag`
    /// 起点（视觉行 = `row − area.y + scroll_y`，视觉列 = `column − area.x`）。
    pub(super) visual: (usize, u16),
    /// Down 时命中测试结果：entry header（可折叠行）或 None。
    /// 冻结命中消除 Up 结算对 wrap_map 的二次反查。
    pub(super) entry_hit: Option<(usize, usize)>, // (slot, local_idx)
}

// ── 吸底自动跟随 ─────────────────────────────────────────────────────────

/// `use_effect` 闭包提取的上下文结构体。
/// 所有 `State<T>` 字段在 mod.rs 闭包外构造时用 `.clone()`（State 是 Arc，clone 是廉价引用拷贝）。
pub(super) struct AutoFollowCtx {
    pub total_visual_rows: usize,
    pub vis_height: u16,
    pub scroll_state: State<ScrollPos>,
    pub prev_items_len: State<usize>,
    pub last_scrolled_at: State<usize>,
    pub items_len: usize,
    pub is_loading: bool,
    /// 粘性吸底开关：用户一向上滚动即 false（浏览模式），滚回真正底部才恢复 true。
    /// 跟随态下内容增长无条件滚底；浏览态下不打扰。
    pub follow_bottom: State<bool>,
    /// 用于检测 resize：total_visual_rows 变化后钳制 scroll_state.offset 到有效范围。
    pub prev_total_visual_rows: State<usize>,
    /// 用于检测 resize：vis_height 变化（终端高度变化）后，若处于跟随态则跟随到底。
    /// use_effect 依赖不含 vis_height，此哨兵负责补上这个缺口。
    pub prev_vis_height: State<u16>,
    /// 用于检测 submit（用户主动发送 prompt）→ 强制滚底，不经过 follow_bottom guard。
    pub loading_epoch: u64,
    pub prev_loading_epoch: State<u64>,
    /// 用于检测 history 切换 / /clear → 重置 prev_items_len/last_scrolled_at，
    /// 触发「新会话首次批量加载」的强制滚底路径。
    pub bridge_reset_counter: u64,
    pub prev_reset_counter: State<u64>,
    /// [Slice 4 §6.8]「等待时锚定此 block」：pending interaction block 的视觉
    /// 行范围（core 行，含起点/终点）。有值时视口对齐到 block 底部（浏览态与
    /// 跟随态均生效——§6.8：不得被新 streaming chunk 滚出视口）；block 完成
    /// （结果回写 → 派生扫描不到 → None）后恢复原语义，不强制 follow
    /// （§15「提交后转为只读结果行且不抢回 viewport」）。
    pub anchor_visual_range: Option<(usize, usize)>,
}

// ── 测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "scroll_test.rs"]
mod tests;
