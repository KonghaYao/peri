//! Chart themes for the storybook `charts` gallery and the GenUI charts.
//!
//! The gallery itself (demo data and widget configuration) lives in the
//! storybook binary. This module maps colors onto a [`tui_chart::Theme`]:
//! [`preview_theme`] is the fixed gallery surface and [`theme_for`] projects the
//! active Peri theme for the `BarChart` / `Sparkline` GenUI widgets, so the
//! storybook and the real message stream draw through the same definitions.

use peri_theme::theme::ThemeDefinition;
use ratatui::style::Color;
use tui_chart::Theme;

/// Series colors, in the order charts assign them to series `0..`.
const SERIES: [Color; 6] = [
    Color::Cyan,
    Color::Yellow,
    Color::Magenta,
    Color::Green,
    Color::LightBlue,
    Color::LightRed,
];

/// Build the chart theme from the storybook palette: black surface, gray text,
/// cyan accent, dark-gray chrome and six distinguishable series colors.
///
/// This is the fixed gallery surface; GenUI charts use [`theme_for`] instead.
pub fn preview_theme() -> Theme {
    let mut theme = Theme::from_base(
        "peri",
        Some(Color::Black),
        Color::Gray,
        Color::Cyan,
        Color::Green,
        Color::Red,
    );
    theme.muted = Color::DarkGray;
    theme.axis = Color::DarkGray;
    theme.grid = Color::DarkGray;
    theme.border = Color::DarkGray;
    theme.title = Color::Gray;
    theme.palette = SERIES.to_vec();
    theme
}

/// Build a chart theme from the active Peri theme.
///
/// There is deliberately no chart background: GenUI charts render inline in the
/// message stream, so painting a surface would only add a block of color behind
/// the rows. Chrome and labels follow the theme's muted/border tokens and the
/// palette keeps the semantic status colors as series colors.
pub fn theme_for(theme: &ThemeDefinition) -> Theme {
    let mut chart = Theme::from_base(
        "peri",
        None,
        theme.component.markdown.text,
        theme.semantic.accent,
        theme.semantic.status.success,
        theme.semantic.status.error,
    );
    chart.background = None;
    chart.muted = theme.semantic.text.muted;
    chart.axis = theme.semantic.border.dim;
    chart.grid = theme.semantic.border.dim;
    chart.border = theme.semantic.border.dim;
    chart.title = theme.semantic.accent;
    let mut palette = Vec::with_capacity(5);
    for color in [
        theme.semantic.accent,
        theme.semantic.status.success,
        theme.semantic.status.warning,
        theme.semantic.status.error,
        theme.semantic.status.running,
    ] {
        if color != Color::Black && !palette.contains(&color) {
            palette.push(color);
        }
    }
    chart.palette = palette;
    chart
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_uses_the_storybook_palette() {
        let theme = preview_theme();
        assert_eq!(theme.name, "peri");
        assert_eq!(theme.background, Some(Color::Black));
        assert_eq!(theme.foreground, Color::Gray);
        assert_eq!(theme.accent, Color::Cyan);
        assert_eq!(theme.muted, Color::DarkGray);
        assert_eq!(theme.axis, Color::DarkGray);
        assert_eq!(theme.grid, Color::DarkGray);
        assert_eq!(theme.border, Color::DarkGray);
    }

    #[test]
    fn theme_palette_has_distinct_series_colors() {
        let theme = preview_theme();
        assert!(theme.palette.len() >= 5, "palette: {:?}", theme.palette);
        assert!(theme.palette.iter().all(|color| *color != Color::Black));
        for (index, color) in theme.palette.iter().enumerate() {
            assert!(
                !theme.palette[index + 1..].contains(color),
                "duplicate palette color {color:?}"
            );
        }
        assert_eq!(theme.color(theme.palette.len()), theme.palette[0]);
    }

    #[test]
    fn production_theme_projects_peri_tokens() {
        let definition = peri_theme::builtin::dark_theme();
        let theme = theme_for(&definition);
        assert_eq!(theme.name, "peri");
        assert_eq!(theme.background, None);
        assert_eq!(theme.foreground, definition.component.markdown.text);
        assert_eq!(theme.accent, definition.semantic.accent);
        assert_eq!(theme.muted, definition.semantic.text.muted);
        assert_eq!(theme.axis, definition.semantic.border.dim);
        assert_eq!(theme.grid, definition.semantic.border.dim);
        assert_eq!(theme.border, definition.semantic.border.dim);
        assert_eq!(theme.title, definition.semantic.accent);
        for color in [
            definition.semantic.accent,
            definition.semantic.status.success,
            definition.semantic.status.warning,
            definition.semantic.status.error,
            definition.semantic.status.running,
        ] {
            if color != Color::Black {
                assert!(theme.palette.contains(&color), "missing {color:?}");
            }
        }
        assert!(theme.palette.iter().all(|color| *color != Color::Black));
        for (index, color) in theme.palette.iter().enumerate() {
            assert!(
                !theme.palette[index + 1..].contains(color),
                "duplicate palette color {color:?}"
            );
        }
    }

    #[test]
    fn theme_base_fields_can_be_overridden() {
        let mut theme = Theme::from_base(
            "plain",
            Some(Color::Black),
            Color::White,
            Color::Blue,
            Color::Green,
            Color::Red,
        );
        theme.muted = Color::DarkGray;
        theme.palette = vec![Color::Cyan];
        theme.title = Color::Gray;
        assert_eq!(theme.muted, Color::DarkGray);
        assert_eq!(theme.title, Color::Gray);
        assert_eq!(theme.color(3), Color::Cyan);
    }
}
