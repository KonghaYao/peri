//! 验证 dark/light 内置主题字段完整性。

use peri_theme::bridge::ThemeDefinitionExt;
use peri_theme::builtin::{dark_theme, light_theme};
use peri_theme::theme::ThemeMode;
use ratatui::style::Color;

#[test]
fn test_dark_theme_fields() {
    let theme = dark_theme();
    assert_eq!(theme.name, "peri-dark");
    assert_eq!(theme.mode, ThemeMode::Dark);

    // Palette 完整性
    assert_eq!(theme.palette.base.bg, Color::Rgb(0, 0, 0));
    assert_eq!(theme.palette.base.fg, Color::Rgb(255, 255, 255));
    assert_eq!(theme.palette.brand.primary, Color::Rgb(215, 119, 87));
    assert_eq!(theme.palette.gray.muted, Color::Rgb(153, 153, 153));
    assert_eq!(theme.palette.gray.dim, Color::Rgb(80, 80, 80));
    assert_eq!(theme.palette.success.primary, Color::Rgb(78, 186, 101));
    assert_eq!(theme.palette.warning.primary, Color::Rgb(255, 193, 7));
    assert_eq!(theme.palette.danger.primary, Color::Rgb(255, 107, 128));
    assert_eq!(theme.palette.info.primary, Color::Rgb(162, 169, 228));

    // Semantic 完整性
    assert_eq!(theme.semantic.text.primary, Color::Rgb(255, 255, 255));
    assert_eq!(theme.semantic.text.muted, Color::Rgb(153, 153, 153));
    assert_eq!(theme.semantic.text.dim, Color::Rgb(80, 80, 80));
    assert_eq!(theme.semantic.border.default, Color::Rgb(80, 80, 80));
    assert_eq!(theme.semantic.border.active, Color::Rgb(215, 119, 87));
    assert_eq!(theme.semantic.accent, Color::Rgb(215, 119, 87));
    assert_eq!(theme.semantic.loading, Color::Rgb(147, 165, 255));
    assert_eq!(theme.semantic.thinking, Color::Rgb(162, 169, 228));
    assert_eq!(theme.semantic.model_info, Color::Rgb(160, 130, 95));
    // Model Panel 档位语义色（dark）
    assert_eq!(theme.semantic.model_accent, Color::Rgb(162, 169, 228));
    assert_eq!(theme.semantic.effort, Color::Rgb(229, 164, 107));
    assert_eq!(theme.semantic.token_context, Color::Rgb(127, 181, 217));
    assert_eq!(theme.semantic.bash_border, Color::Rgb(253, 93, 177));

    // Component 完整性
    assert_eq!(theme.component.message.user_bg, Color::Rgb(55, 55, 55));
    assert_eq!(theme.component.panel.min_height, 8);
    assert_eq!(theme.component.panel.max_height, 28);
    assert_eq!(theme.component.popup.modal_max_width, 90);
    assert_eq!(theme.component.popup.modal_max_height, 28);
    assert_eq!(theme.component.popup.inline_height, 10);
    // 会话标题底色板：8 色，深色系（配白字）
    let dark_palette = theme.component.input.session_title_palette;
    assert_eq!(dark_palette.len(), 8);
    assert_eq!(dark_palette[0], Color::Rgb(18, 52, 26));
    assert!(dark_palette.iter().all(|c| matches!(c, Color::Rgb(..))));
    assert_eq!(
        theme.component.statusbar.resource_good,
        Color::Rgb(78, 186, 101)
    );

    // Diff 色值（统一为 DEFAULT_THEME 的 SAGE/ERROR/THINKING）
    assert_eq!(theme.palette.diff.add, Color::Rgb(78, 186, 101));
    assert_eq!(theme.palette.diff.remove, Color::Rgb(255, 107, 128));
    assert_eq!(theme.palette.diff.hunk, Color::Rgb(162, 169, 228));
}

#[test]
fn test_light_theme_fields() {
    let theme = light_theme();
    assert_eq!(theme.name, "peri-light");
    assert_eq!(theme.mode, ThemeMode::Light);

    // 浅色背景
    assert_eq!(theme.palette.base.bg, Color::Rgb(250, 250, 250));
    assert_eq!(theme.palette.base.fg, Color::Rgb(46, 46, 42));
    assert_eq!(theme.semantic.text.primary, Color::Rgb(46, 46, 42));
    assert_eq!(theme.semantic.surface.default, Color::Rgb(245, 245, 248));
    assert_eq!(theme.component.message.user_bg, Color::Rgb(230, 230, 235));
    // 会话标题底色板：浅色系（配黑字）
    let light_palette = theme.component.input.session_title_palette;
    assert_eq!(light_palette.len(), 8);
    assert_eq!(light_palette[0], Color::Rgb(210, 230, 215));
    // Model Panel 档位语义色（light 可读性变体）
    assert_eq!(theme.semantic.model_accent, Color::Rgb(107, 114, 201));
    assert_eq!(theme.semantic.effort, Color::Rgb(176, 111, 46));
    assert_eq!(theme.semantic.token_context, Color::Rgb(62, 138, 184));
}

#[test]
fn test_dark_palette_mapping() {
    let theme = dark_theme();
    let palette = theme.to_palette();

    assert_eq!(palette.fg, Color::Rgb(255, 255, 255));
    assert_eq!(palette.fg_dim, Color::Rgb(153, 153, 153));
    assert_eq!(palette.bg, Color::Rgb(0, 0, 0));
    assert_eq!(palette.accent, Color::Rgb(215, 119, 87));
    assert_eq!(palette.success, Color::Rgb(78, 186, 101));
    assert_eq!(palette.warning, Color::Rgb(255, 193, 7));
    assert_eq!(palette.error, Color::Rgb(255, 107, 128));
    assert_eq!(palette.info, Color::Rgb(162, 169, 228));
    assert_eq!(palette.placeholder, Color::Rgb(153, 153, 153));
}

#[test]
fn test_dark_peri_colors_mapping() {
    let theme = dark_theme();
    let peri = theme.to_peri_colors();

    assert_eq!(peri.surface_user, Color::Rgb(55, 55, 55));
    assert_eq!(peri.surface_popup, Color::Rgb(0, 0, 0));
    assert_eq!(peri.surface_cursor, Color::Rgb(38, 38, 38));
    assert_eq!(peri.status_running, Color::Rgb(125, 207, 255)); // #7DCFFF（§4 表）
    assert_eq!(peri.model_info, Color::Rgb(160, 130, 95));
    assert_eq!(peri.bash_border, Color::Rgb(253, 93, 177));
    assert_eq!(peri.selected_fg, Color::Rgb(178, 185, 249));
    assert_eq!(peri.diff_add, Color::Rgb(78, 186, 101));
    assert_eq!(peri.diff_remove, Color::Rgb(255, 107, 128));
    assert_eq!(peri.diff_hunk, Color::Rgb(162, 169, 228));
    assert_eq!(peri.resource_good, Color::Rgb(78, 186, 101));
    assert_eq!(peri.resource_warn, Color::Rgb(255, 193, 7));
    assert_eq!(peri.resource_bad, Color::Rgb(255, 107, 128));
}
