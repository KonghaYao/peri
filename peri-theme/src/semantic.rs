//! 语义令牌：表达颜色用途。
//!
//! 定义文字、边框、状态、表面、Diff 五种语义令牌。
//! 每类令牌在主题定义中映射调色板的具体色值。

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// 语义令牌集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTokens {
    pub accent: Color,
    pub text: TextTokens,
    pub border: BorderTokens,
    pub status: StatusTokens,
    pub surface: SurfaceTokens,
    pub diff: DiffTokens,
    pub loading: Color,
    pub thinking: Color,
    pub model_info: Color,
    pub bash_border: Color,
    pub selected_fg: Color,
}

/// 文字三级亮度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextTokens {
    pub primary: Color,
    pub muted: Color,
    pub dim: Color,
}

/// 边框三级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BorderTokens {
    pub default: Color,
    pub active: Color,
    pub dim: Color,
}

/// 状态四色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusTokens {
    pub running: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

/// 表面五层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceTokens {
    pub default: Color,
    pub user: Color,
    pub popup: Color,
    pub selection: Color,
    pub cursor: Color,
}

/// Diff 语义 7 色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffTokens {
    pub add: Color,
    pub remove: Color,
    pub hunk: Color,
    pub add_bg: Color,
    pub remove_bg: Color,
    pub add_word_bg: Color,
    pub remove_word_bg: Color,
}
