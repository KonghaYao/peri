//! 主题定义：ThemeMode + ThemeDefinition。
//!
//! 顶级结构，聚合 Palette、SemanticTokens、ComponentTokens 为一个完整主题。

use serde::{Deserialize, Serialize};

use crate::component::ComponentTokens;
use crate::palette::Palette;
use crate::semantic::SemanticTokens;

/// 主题模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    Dark,
    Light,
    HighContrast,
}

/// 完整主题定义。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeDefinition {
    /// 主题名称（如 "peri-dark"、"peri-light"）。
    pub name: String,
    /// 模式（Dark / Light / HighContrast）。
    pub mode: ThemeMode,
    /// 原始调色板。
    pub palette: Palette,
    /// 语义令牌。
    pub semantic: SemanticTokens,
    /// 组件令牌。
    pub component: ComponentTokens,
}
