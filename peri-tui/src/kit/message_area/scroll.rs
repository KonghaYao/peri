//! 滚轮事件处理 + 吸底自动跟随。

mod auto_follow;
mod event;
mod geometry;
mod gesture;
mod throttle;

#[cfg(test)]
#[path = "scroll_test.rs"]
mod tests;

use self::auto_follow::update_follow_on_scroll;
#[cfg(test)]
use self::auto_follow::{
    anchor_scroll_target, consume_reset_force_bottom, should_follow_after_user_scroll,
};
#[cfg(test)]
use self::gesture::{entry_click_target, freeze_down, is_click, is_click_for};
use self::gesture::{selection_bounds_on_release, selection_captures_pointer, selection_position};
use self::throttle::apply_scroll;
#[cfg(test)]
use self::throttle::{apply_delta_to_offset, land_scroll_delta};

#[cfg(test)]
use crate::kit::message_area::props::ScrollbarFields;
#[cfg(test)]
use crate::kit::message_area::selection::WrappedLineInfo;

#[cfg(test)]
use ratatui_kit::ratatui::layout::Rect;

mod scroll_state;
pub(super) use scroll_state::AutoFollowCtx;
pub(super) use scroll_state::GesturePending;
pub(super) use scroll_state::ScrollPos;

pub(super) use self::auto_follow::{new_output_indicator_active, run_auto_follow};
pub(super) use self::event::handle_event;
pub(super) use self::geometry::{ScrollbarDragState, is_scrollbar_column};
pub(super) use self::gesture::{
    DragAction, drag_step, freeze_down_index, release_upgrades, settle_up,
    text_selection_accepts_pointer,
};
pub(super) use self::throttle::{DragThrottle, SCROLL_PADDING};
pub(crate) use self::throttle::{SCROLL_LINES, scroll_frame_ms};
