//! 输入区域 `#[component]` 组件。
//!
//! S8：完整输入体验——
//! - **多行 buffer**：Shift/Alt+Enter 换行；渲染按行拆分；高度动态扩展（3~40% 屏幕）
//! - **history**：Up/Down 浏览 `INPUT_HISTORY` atom；Esc 或回到栈底恢复编辑态
//! - **@mention**：输入 @ 触发 AT_MENTION_ACTIVE；popup 显示在输入框上方
//! - **slash**：行首 / 触发 SLASH_HINT_ACTIVE；popup 显示在输入框上方
//! - **提交**：Enter 提交，submit_consumer 消费 + push_history
mod attachment;
mod hooks;
mod image;
mod paste;
mod popup;
mod render;
mod submit;

#[cfg(test)]
#[path = "input_area_test.rs"]
mod tests;

#[cfg(test)]
use crate::components::textarea::TextAreaState;
#[cfg(test)]
use crate::i18n;
#[cfg(test)]
use attachment::AttachmentEdge;
#[cfg(test)]
use popup::{apply_slash_selection, build_slash_items, detect_slash_token};
#[cfg(test)]
use ratatui_kit::ratatui::text::Line;
#[cfg(test)]
use render::{build_session_title_line, readable_fg, stable_hash, truncate_title_to_width};

#[cfg(test)]
use crate::kit::message_area::grid::GridSpec;
#[cfg(test)]
use crate::kit::slash_completion::SlashCompletionItem;
#[cfg(test)]
use fluent_bundle::FluentValue;

#[cfg(test)]
use crate::kit::slash_completion::{ConfirmOutcome, SlashActionKind, confirm_outcome};
#[cfg(test)]
use ratatui_kit::ratatui::widgets::Block;
#[cfg(test)]
use submit::submit_text;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and names the implementation leaf after its primary type."
)]
mod input_area;
use self::input_area as composer;
pub use composer::*;

#[cfg(test)]
pub(crate) use popup::get_cached_slash_items;
pub(crate) use popup::refresh_slash_items;
pub(crate) use submit::is_remote_command;
pub(crate) use submit::send_local_user_bubble;

#[cfg(test)]
use crate::kit::atoms::ACTIVE_PANEL;

#[cfg(test)]
use crate::kit::atoms::AT_MENTION_ACTIVE;

#[cfg(test)]
use crate::kit::atoms::AVAILABLE_SLASH_COMMANDS;

#[cfg(test)]
use crate::kit::atoms::MENTION_PREFIX;

#[cfg(test)]
use crate::kit::atoms::SLASH_HINT_ACTIVE;

#[cfg(test)]
use crate::kit::atoms::FILE_LIST;
#[cfg(test)]
use crate::kit::atoms::SLASH_PREFIX;

#[cfg(test)]
use crate::kit::atoms::WIZARD_ACTIVE;

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(super) use test_support::reset_submit_side_effect_state;

#[cfg(test)]
use self::attachment::remove_pending_attachment;
