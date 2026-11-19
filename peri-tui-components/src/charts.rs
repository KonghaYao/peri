//! Chart theme shared by the storybook `charts` gallery.
//!
//! The gallery itself (demo data and widget configuration) lives in the
//! storybook binary; this module only maps the Peri palette onto a
//! [`tui_chart::Theme`] so every chart uses the same surface, chrome and
//! series colors.

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
