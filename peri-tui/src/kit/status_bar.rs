//! ratatui-kit StatusBar component.

use std::time::Instant;
use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Flex},
        style::Stylize,
        text::Line,
        widgets::Paragraph,
    },
};
use crate::kit::atoms;
use crate::ui::theme;

/// 状态栏第 1 行：名称/model/provider/权限/MEM/上下文使用率
#[component]
fn StatusBarRow1(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let acp = hooks.use_store(*atoms::ACP_STATE.get().unwrap());
    let model_hl = hooks.use_store(*atoms::MODEL_HIGHLIGHT_UNTIL.get().unwrap());
    let mode_hl = hooks.use_store(*atoms::MODE_HIGHLIGHT_UNTIL.get().unwrap());

    let state = acp.read(); // AcpStateSnapshot 非 Copy
    let _model_highlight = model_hl.get().map(|t| t > Instant::now()).unwrap_or(false);
    let _mode_highlight = mode_hl.get().map(|t| t > Instant::now()).unwrap_or(false);

    // 构建第 1 行：名称占位（后续 Phase 8 由 ACP_STATE 补充字段）
    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
        ) {
            Text(text: Paragraph::new(Line::from("peri").fg(theme::THINKING).bold()))
            Text(text: Paragraph::new(Line::from(format!(" {} agents", state.view_count)).fg(theme::MUTED)))
        }
    )
}

/// 状态栏第 2 行：左侧状态 + 右侧快捷键
#[component]
fn StatusBarRow2(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let popup = hooks.use_store(*atoms::POPUP_ACTIVE.get().unwrap());
    let at_active = hooks.use_store(*atoms::AT_MENTION_ACTIVE.get().unwrap());
    let slash_active = hooks.use_store(*atoms::SLASH_HINT_ACTIVE.get().unwrap());

    let is_popup = popup.get();
    let is_at = at_active.get();
    let is_slash = slash_active.get();

    // 快捷键提示：根据当前状态切换
    let hints = if is_popup {
        Line::from(" Esc: close | Enter: confirm ").fg(theme::MUTED)
    } else if is_at || is_slash {
        Line::from(" Esc: close | Tab: navigate | Enter: select ").fg(theme::MUTED)
    } else {
        Line::from(" /: commands | Shift+Enter: newline | Ctrl+T: model | Ctrl+O: diff ").fg(theme::MUTED)
    };

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
            justify_content: Flex::Center,
        ) {
            Text(text: Paragraph::new(hints).centered())
        }
    )
}

#[component]
pub fn StatusBar(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(3),
        ) {
            StatusBarRow1()
            StatusBarRow2()
            // 第 3 行留空（缓冲行）
            Text(text: Paragraph::new(Line::from("")))
        }
    )
}
