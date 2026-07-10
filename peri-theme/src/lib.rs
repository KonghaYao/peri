//! `peri-theme` — 统一主题系统。
//!
//! 提供主题定义、内置主题、JSON loader、bridge 和全局 Atom。

pub mod atoms;
pub mod bridge;
pub mod builtin;
pub mod component;
pub mod loader;
pub mod palette;
pub mod peri_colors;
pub mod semantic;
pub mod theme;

/// Prelude: 常用类型的便捷 re-export。
pub mod prelude {
    pub use crate::atoms::{PALETTE_ATOM, PERI_COLORS_ATOM, THEME_ATOM, init_theme_atoms};
    pub use crate::bridge::{ThemeDefinitionExt, default_palette, default_peri_colors};
    pub use crate::builtin::{dark_theme, light_theme};
    pub use crate::component::ComponentTokens;
    pub use crate::loader::load_theme;
    pub use crate::palette::Palette;
    pub use crate::peri_colors::PeriColors;
    pub use crate::semantic::SemanticTokens;
    pub use crate::theme::{ThemeDefinition, ThemeMode};
}
