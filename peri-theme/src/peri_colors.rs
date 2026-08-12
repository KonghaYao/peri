//! 精简版 PeriColors：ratatui-kit Palette 未覆盖的特有颜色（~21 字段）。
//!
//! 不重复 Palette 已有的字段（如 fg_dim=text_dim、accent、border_active 等）。
//! 布局数值（u16）保留在 ComponentTokens，不进入此结构。

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// 精简版 PeriColors：ratatui-kit Palette 未覆盖的特有语义色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriColors {
    // ── Surface 五层（Palette 只有 surface，Peri 有 5 层） ──
    pub surface_user: Color,
    pub surface_popup: Color,
    pub surface_cursor: Color,
    /// 抬升表面（composer、expanded tool body，§4 表）。
    #[serde(default)]
    pub surface_raised: Color,
    /// 下沉表面（code、terminal output，§4 表）。
    #[serde(default)]
    pub surface_sunken: Color,

    // ── 状态扩展（Palette 只有 success/warning/error） ──
    pub status_running: Color,
    pub status_thinking: Color,

    // ── 消息流角色强调色（§4 表）──
    #[serde(default)]
    pub accent_user: Color,
    #[serde(default)]
    pub accent_assistant: Color,
    #[serde(default)]
    pub accent_reasoning: Color,
    #[serde(default)]
    pub accent_tool: Color,

    // ── 文字层级扩展 ──
    #[serde(default)]
    pub text_secondary: Color,

    // ── 语法语义色（§4 表）──
    #[serde(default)]
    pub syntax_command: Color,
    #[serde(default)]
    pub syntax_path: Color,

    // ── 特殊语义 ──
    pub border_dim: Color,
    pub model_info: Color,
    pub bash_border: Color,
    pub selected_fg: Color,

    // ── Diff 7 色 ──
    pub diff_add: Color,
    pub diff_remove: Color,
    pub diff_hunk: Color,
    pub diff_add_bg: Color,
    pub diff_remove_bg: Color,
    pub diff_add_word_bg: Color,
    pub diff_remove_word_bg: Color,

    // ── Scrollbar ──
    pub scrollbar_thumb: Color,
    pub scrollbar_track: Color,

    // ── StatusBar 资源色（常被使用） ──
    pub resource_good: Color,
    pub resource_warn: Color,
    pub resource_bad: Color,
}

impl Default for PeriColors {
    fn default() -> Self {
        Self {
            surface_user: Color::Reset,
            surface_popup: Color::Reset,
            surface_cursor: Color::Reset,
            surface_raised: Color::Reset,
            surface_sunken: Color::Reset,
            status_running: Color::Reset,
            status_thinking: Color::Reset,
            accent_user: Color::Reset,
            accent_assistant: Color::Reset,
            accent_reasoning: Color::Reset,
            accent_tool: Color::Reset,
            text_secondary: Color::Reset,
            syntax_command: Color::Reset,
            syntax_path: Color::Reset,
            border_dim: Color::Reset,
            model_info: Color::Reset,
            bash_border: Color::Reset,
            selected_fg: Color::Reset,
            diff_add: Color::Reset,
            diff_remove: Color::Reset,
            diff_hunk: Color::Reset,
            diff_add_bg: Color::Reset,
            diff_remove_bg: Color::Reset,
            diff_add_word_bg: Color::Reset,
            diff_remove_word_bg: Color::Reset,
            scrollbar_thumb: Color::Reset,
            scrollbar_track: Color::Reset,
            resource_good: Color::Reset,
            resource_warn: Color::Reset,
            resource_bad: Color::Reset,
        }
    }
}
