//! ratatui-kit AskUserPanel component.
//!
//! 用户问答面板——当 agent 调用 AskUserQuestion 工具时，自动作为 Panel 内联渲染
//! 在 MessageArea 和 InputArea 之间（替代弹窗形式）。
//!
//! 面板逻辑沿用列表导航（Tab 切换问题、↑/↓ 移动高亮）与提交/拒绝路径，
//! 通过 panel_shell! 渲染。

mod form;
mod typing;

#[cfg(test)]
#[path = "ask_user_test.rs"]
mod tests;

#[cfg(test)]
use crate::kit::acp_types::PendingInteraction;

#[cfg(test)]
use form::FormOutcome;
#[cfg(test)]
use form::FormState;
#[cfg(test)]
use form::build_answers_map;

mod ask_user_panel;
pub use ask_user_panel::*;
