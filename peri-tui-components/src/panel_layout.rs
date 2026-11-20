//! Pure panel size layout shared by the overlay and first-frame predictions.
//!
//! Panel identity, scroll registration and invalidation stay in the application.
//! This module only answers how many rows a panel may use and how many of those
//! rows are content once the shell borders are removed.

/// Rows always left for the transcript when a panel opens.
pub const MESSAGE_RESERVE: u16 = 4;

/// Rows consumed by the panel shell's top and bottom borders.
pub const PANEL_BORDER_ROWS: u16 = 2;

/// Outer panel height for a terminal height and theme limits.
///
/// The transcript keeps [`MESSAGE_RESERVE`] rows. On terminals too short for
/// the reserve the result saturates at zero instead of underflowing; the theme
/// limits cannot exceed the rows actually available.
#[must_use]
pub fn panel_height(term_height: u16, min_height: u16, max_height: u16) -> u16 {
    let available = term_height.saturating_sub(MESSAGE_RESERVE);
    max_height.min(available).max(min_height.min(available))
}

/// Content rows inside a panel of `panel_height` rows (borders excluded).
#[must_use]
pub fn content_rows_for_height(panel_height: u16) -> u16 {
    panel_height.saturating_sub(PANEL_BORDER_ROWS)
}

/// Content rows predicted from the terminal height before the panel is measured.
#[must_use]
pub fn panel_content_rows(term_height: u16, min_height: u16, max_height: u16) -> u16 {
    content_rows_for_height(panel_height(term_height, min_height, max_height))
}

#[cfg(test)]
mod tests {
    use super::{content_rows_for_height, panel_content_rows, panel_height};

    #[test]
    fn theme_limits_bound_the_panel() {
        assert_eq!(panel_height(30, 10, 20), 20, "max wins below available");
        assert_eq!(panel_height(30, 10, 100), 26, "reserve keeps 4 rows");
        assert_eq!(
            panel_height(14, 10, 20),
            10,
            "min wins when available is smaller"
        );
    }

    #[test]
    fn short_terminals_saturate() {
        assert_eq!(panel_height(0, 10, 20), 0);
        assert_eq!(panel_height(4, 10, 20), 0, "reserve consumes the screen");
        assert_eq!(panel_height(5, 10, 20), 1);
        assert_eq!(content_rows_for_height(0), 0);
        assert_eq!(content_rows_for_height(1), 0);
        assert_eq!(content_rows_for_height(2), 0);
        assert_eq!(content_rows_for_height(3), 1);
    }

    #[test]
    fn first_frame_matches_measured_height() {
        for term_height in 0..=60 {
            let height = panel_height(term_height, 10, 20);
            let predicted = panel_content_rows(term_height, 10, 20);
            let measured = content_rows_for_height(height);
            // The measured frame subtracts the same two border rows.
            assert_eq!(predicted, measured, "term height {term_height}");
        }
    }

    #[test]
    fn content_is_the_outer_height_without_borders() {
        assert_eq!(content_rows_for_height(18), 16);
        assert_eq!(panel_content_rows(30, 10, 20), 18);
    }
}
