//! MessageArea：直接读取 VIEW_MODELS，通过 vm_to_lines 将 TuiRenderUnit
//! 转换为 Vec<Line>，按视口裁剪后渲染。
//!
//! - 滚动：由自持 ScrollPos（usize 偏移）处理键盘/鼠标事件（offset 管理）
//! - 渲染：视口裁剪——只 clone + highlight + 渲染视口内 ~60 行，避免 O(N×W) per render
//! - 智能跟随：use_effect 检测 VIEW_MODELS 变化
//! - 不再使用 RENDER_CACHE / render_bridge / ScrollView / wrap_map（已替换为 wrap_map_cache）

mod entry_nav;
mod footer;
mod genui_action;
pub(crate) mod grid;
mod handlers;
mod hits;
mod image_action;
mod props;
pub(crate) mod render;
pub(crate) mod scroll;
pub(crate) mod selection;
mod slot_index;
mod transcript;
use self::slot_index as transcript_index;
mod viewport_anchor;
mod vm_cache_slot;
use self::vm_cache_slot as vm_cache;

#[cfg(test)]
#[path = "center_band_test.rs"]
mod center_band_tests;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "viewport_test.rs"]
mod viewport_tests;

#[cfg(test)]
#[path = "window_viewport_test.rs"]
mod window_viewport_tests;

#[cfg(test)]
#[path = "p_matrix_tui_test.rs"]
mod p_matrix_tui_tests;

#[cfg(test)]
#[path = "mermaid_viewport_test.rs"]
mod mermaid_viewport_tests;

#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use self::entry_nav::{
    apply_fold_override, apply_fold_toggle, apply_group_member_toggle, cycle_interaction_option,
    entry_click_decision, fold_key_of, pending_interaction_of, set_entry_focus,
};
#[cfg(test)]
use self::handlers::{ImagePreviewHoverGate, schedule_image_preview_hover};
#[cfg(test)]
use self::hits::CopyButtonHit;
#[cfg(test)]
use self::hits::ImageLineHit;

#[cfg(test)]
use self::image_action::hover_target_for;
#[cfg(all(test, target_os = "macos"))]
use self::image_action::{build_open_command, build_open_command_with, try_open_image};

#[cfg(test)]
use self::vm_cache::total_visual_rows;

#[cfg(test)]
use crate::kit::tui_render_unit::TuiRenderUnit;
#[cfg(test)]
use crate::kit::tui_render_unit::{FoldKey, FoldState};

#[cfg(test)]
use footer::KeepGoingLayout;

#[cfg(test)]
use ratatui_kit::ratatui::text::Line;

#[cfg(test)]
use scroll::GesturePending;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and names the implementation leaf after its primary type."
)]
mod message_area;
pub use message_area::*;

pub(crate) use self::hits::ImageHoverState;
pub(crate) use footer::hash_todo_items;
pub use footer::{TodoItem, TodoStatus};
pub use props::MessageAreaProps;
#[cfg(test)]
pub(crate) use selection::run_synthetic_slot_index;
pub(crate) use viewport_anchor::window_identity_token;

#[cfg(test)]
use crate::kit::atoms::FocusedEntry;
