//! Bridge：ThemeDefinition → ratatui-kit Palette / PeriColors 映射。
//!
//! 提供 `to_palette()` 和 `to_peri_colors()` 两个转换方法。

use ratatui_kit::prelude::Palette;

use crate::peri_colors::PeriColors;
use crate::theme::ThemeDefinition;

#[path = "bridge/theme_definition_ext.rs"]
mod theme_definition_ext;
pub use theme_definition_ext::ThemeDefinitionExt;
pub use theme_definition_ext::default_palette;
pub use theme_definition_ext::default_peri_colors;
