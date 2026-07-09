//! Markdown 主题色值 → ratatui-kit Palette 映射。
//!
//! 将 peri-tui 原有的 hardcoded 色值映射到 ratatui-kit PaletteProvider，
//! Markdown / CodeBlock 组件通过 use_component_theme 自动派生色值。

use ratatui::style::Color;
use ratatui_kit::prelude::Palette;

/// 构建 peri-tui 专用的 markdown 色板。
///
/// 映射关系（对应原 DefaultMarkdownTheme）：
///
/// | 色值（#hex）     | 当前用途             | Palette 槽位    |
/// |------------------|----------------------|-----------------|
/// | #FFFFFF          | text / list_bullet   | Palette::fg     |
/// | #999999          | muted / quote / sep  | Palette::fg_dim |
/// | #4EBA65          | link / code_prefix   | Palette::success|
/// | #FFC107          | heading              | Palette::warning|
/// | #A2A9E4          | code                 | Palette::info   |
pub fn peri_markdown_palette() -> Palette {
    let mut p = Palette::default();
    p.fg = Color::Rgb(255, 255, 255); // #FFFFFF
    p.fg_dim = Color::Rgb(153, 153, 153); // #999999
    p.success = Color::Rgb(78, 186, 101); // #4EBA65
    p.warning = Color::Rgb(255, 193, 7); // #FFC107
    p.info = Color::Rgb(162, 169, 228); // #A2A9E4
    p
}
