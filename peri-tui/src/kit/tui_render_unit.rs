//! TuiRenderUnit —— TUI 内部渲染单元类型，不共享给 ACP 层。

// ---------------------------------------------------------------------------
// PartialEq 辅助宏——跳过 content_hash 字段
// ---------------------------------------------------------------------------

/// Implement `PartialEq` for a struct, comparing only the listed fields
/// (excluding `content_hash`).
macro_rules! tui_impl_partial_eq {
    ($ty:ty: $($field:ident),+ $(,)?) => {
        impl PartialEq for $ty {
            fn eq(&self, other: &Self) -> bool {
                $(self.$field == other.$field)&&+
            }
        }
    };
}

mod bubble;
mod diff;
mod fold;
mod group;
mod hash;
mod interaction;
mod reminder;
mod tool_card;
mod unit;

pub use bubble::{TuiAssistantBubble, TuiReasoningBlock, TuiUserBubble};
pub use diff::{TuiDiffBlock, TuiHunk, TuiHunkLine, TuiHunkLineKind, diff_change_counts};
pub use fold::{
    EntryStatus, FoldKey, FoldState, FoldTarget, entry_status_code, fold_for_status,
    fold_state_code,
};
pub use group::{
    TuiCollapsedGroup, TuiDivider, TuiNoteLevel, TuiSubAgentGroup, TuiSystemNote, TuiTodoSummary,
};
pub use hash::{tui_hash_combine, tui_hash_roll, tui_hash_roll_update, tui_hash_str};
pub use interaction::{InteractionKind, TuiAskUserBlock, TuiAskUserItem, interaction_kind_code};
pub use reminder::{ReminderInfo, ReminderType, detect_reminder};
pub use tool_card::{
    TuiSkillPresentation, TuiTodoChange, TuiTodoChangeKind, TuiTodoItem, TuiTodoPresentation,
    TuiTodoStatus, TuiToolCard, TuiToolPresentation,
};
pub use unit::TuiRenderUnit;

#[cfg(test)]
use crate::i18n;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "tui_render_unit_test.rs"]
mod tests;
