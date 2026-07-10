//! 组件令牌：定义每个 TUI 组件的样式。
//!
//! 语义令牌层之上的一层。PanelTokens、PopupTokens 含布局数值
//! （`min_height`、`max_width` 等 u16），这些是组件层的结构定义，
//! 不进入 PeriColors。

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// 组件令牌集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentTokens {
    pub message: MessageTokens,
    pub input: InputTokens,
    pub panel: PanelTokens,
    pub popup: PopupTokens,
    pub statusbar: StatusBarTokens,
    pub markdown: MarkdownTokens,
    pub scrollbar: ScrollbarTokens,
}

/// 消息气泡样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageTokens {
    pub user_bg: Color,
    pub ai_prefix: Color,
    pub tool_indicator: Color,
    pub reasoning: Color,
}

/// 输入区样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputTokens {
    pub border: Color,
    pub border_loading: Color,
    pub cursor_fg: Color,
    pub cursor_bg: Color,
    pub prompt: Color,
    pub prompt_loading: Color,
    pub continuation: Color,
    pub placeholder: Color,
}

/// 面板样式（含布局数值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelTokens {
    pub border: Color,
    pub title: Color,
    pub row_selected: Color,
    pub min_height: u16,
    pub max_height: u16,
}

/// 弹窗样式（含布局数值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PopupTokens {
    pub bg: Color,
    pub border: Color,
    pub action_primary: Color,
    pub selected_fg: Color,
    pub modal_max_width: u16,
    pub modal_max_height: u16,
    pub inline_height: u16,
}

/// 状态栏样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusBarTokens {
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    pub mode_accept_edit: Color,
    pub mode_auto: Color,
    pub mode_bypass: Color,
    pub resource_good: Color,
    pub resource_warn: Color,
    pub resource_bad: Color,
}

/// Markdown 渲染样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownTokens {
    pub text: Color,
    pub code: Color,
    pub quote: Color,
}

/// 滚动条样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrollbarTokens {
    pub thumb: Color,
    pub track: Color,
}
