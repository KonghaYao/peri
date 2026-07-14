//! 调色板：原始颜色层，不含语义。
//!
//! 所有基础色值定义在这一层。Base/Gray/State/Diff 四组，
//! 每组只存颜色数值，不表达用途。

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// 完整调色板——原始颜色层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    pub base: BasePalette,
    pub brand: StatePalette,
    pub gray: GrayPalette,
    pub accent: StatePalette,
    pub success: StatePalette,
    pub warning: StatePalette,
    pub danger: StatePalette,
    pub info: StatePalette,
    pub diff: DiffPalette,
}

/// 基础色（背景 + 前景）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BasePalette {
    pub bg: Color,
    pub fg: Color,
}

/// 灰度层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrayPalette {
    pub bright: Color,
    pub muted: Color,
    pub dim: Color,
    pub dark: Color,
}

/// 单色状态色（品牌/强调/成功/警告/危险/信息）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatePalette {
    pub primary: Color,
}

/// Diff 专用 7 色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffPalette {
    pub add: Color,
    pub remove: Color,
    pub hunk: Color,
    pub add_bg: Color,
    pub remove_bg: Color,
    pub add_word_bg: Color,
    pub remove_word_bg: Color,
}
