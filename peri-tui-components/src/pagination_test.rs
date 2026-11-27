use super::*;
use peri_theme::builtin::dark_theme;

const LABELS: PaginationLabels<'static> = PaginationLabels {
    previous: "‹ Prev",
    next: "Next ›",
};

fn row(width: u16) -> Rect {
    Rect::new(0, 0, width, 1)
}

fn state(page: usize, page_count: usize, has_more: bool) -> PaginationState {
    PaginationState {
        page,
        page_count,
        has_more,
    }
}

fn labels_of(layout: &PaginationLayout) -> Vec<&str> {
    layout
        .controls
        .iter()
        .map(|control| control.label.as_str())
        .collect()
}

fn pages_of(layout: &PaginationLayout) -> Vec<usize> {
    layout
        .controls
        .iter()
        .filter_map(|control| match control.action {
            PageAction::Page(page) => Some(page),
            _ => None,
        })
        .collect()
}

fn rect_text(buf: &Buffer, rect: Rect) -> String {
    (rect.x..rect.right())
        .map(|x| buf[(x, rect.y)].symbol())
        .collect()
}

#[test]
fn all_pages_are_shown_when_they_fit() {
    let layout = pagination_layout(row(40), state(1, 4, false), LABELS);
    assert_eq!(
        labels_of(&layout),
        vec!["‹ Prev", "1", "2", "3", "4", "Next ›"]
    );
    assert_eq!(
        layout.controls.iter().map(|c| c.action).collect::<Vec<_>>(),
        vec![
            PageAction::Previous,
            PageAction::Page(0),
            PageAction::Page(1),
            PageAction::Page(2),
            PageAction::Page(3),
            PageAction::Next,
        ]
    );
    assert_eq!(
        layout.controls.iter().map(|c| c.rect).collect::<Vec<_>>(),
        vec![
            Rect::new(0, 0, 6, 1),
            Rect::new(7, 0, 1, 1),
            Rect::new(9, 0, 1, 1),
            Rect::new(11, 0, 1, 1),
            Rect::new(13, 0, 1, 1),
            Rect::new(15, 0, 6, 1),
        ]
    );
    assert_eq!(layout.width, 21);
}

#[test]
fn middle_window_keeps_first_last_current_and_neighbours() {
    let layout = pagination_layout(row(80), state(24, 50, false), LABELS);
    assert_eq!(
        labels_of(&layout),
        vec![
            "‹ Prev", "1 ", "…", "23", "24", "25", "26", "27", "…", "50", "Next ›",
        ]
    );
    assert_eq!(pages_of(&layout), vec![0, 22, 23, 24, 25, 26, 49]);
    let gaps = layout
        .controls
        .iter()
        .filter(|control| control.action == PageAction::Gap)
        .count();
    assert_eq!(gaps, 2);
    assert!(
        layout
            .controls
            .iter()
            .any(|control| control.current && control.action == PageAction::Page(24))
    );
    assert!(
        layout
            .controls
            .iter()
            .filter(|control| matches!(control.action, PageAction::Page(_)))
            .all(|control| control.rect.width == 2),
        "every page control is as wide as the highest page"
    );
}

#[test]
fn start_and_end_windows_only_gap_on_the_open_side() {
    let start = pagination_layout(row(80), state(0, 20, false), LABELS);
    assert_eq!(
        labels_of(&start),
        vec!["‹ Prev", "1 ", "2 ", "3 ", "…", "20", "Next ›"]
    );
    assert_eq!(
        start.controls[1].rect.width, 2,
        "page controls share digits"
    );

    let end = pagination_layout(row(80), state(19, 20, false), LABELS);
    assert_eq!(
        labels_of(&end),
        vec!["‹ Prev", "1 ", "…", "18", "19", "20", "Next ›"]
    );
}

#[test]
fn page_numbers_are_unique_with_the_current_page_included() {
    for page_count in [1usize, 2, 7, 8, 9, 20, 50, 500] {
        for page in 0..page_count {
            let layout = pagination_layout(row(200), state(page, page_count, true), LABELS);
            let numbers = pages_of(&layout);
            assert!(
                numbers.windows(2).all(|pair| pair[0] < pair[1]),
                "page numbers must stay strictly increasing for {page}/{page_count}"
            );
            assert!(
                numbers.contains(&page),
                "the current page must be drawn for {page}/{page_count}"
            );
            assert!(numbers.len() <= MAX_PAGE_NUMBERS);
            if page_count > MAX_PAGE_NUMBERS {
                assert_eq!(numbers.first().copied(), Some(0));
                assert_eq!(numbers.last().copied(), Some(page_count - 1));
            }
        }
    }
}

#[test]
fn ellipses_cover_exactly_the_skipped_ranges() {
    for page_count in [8usize, 9, 20, 500] {
        for page in 0..page_count {
            let layout = pagination_layout(row(200), state(page, page_count, false), LABELS);
            let controls = &layout.controls;
            assert_ne!(controls[0].action, PageAction::Gap);
            assert_ne!(controls[controls.len() - 1].action, PageAction::Gap);
            for pair in controls.windows(2) {
                if let (PageAction::Page(a), PageAction::Page(b)) = (pair[0].action, pair[1].action)
                {
                    assert_eq!(b, a + 1, "adjacent numbers need no ellipsis");
                }
            }
            for triple in controls.windows(3) {
                if let (PageAction::Page(a), PageAction::Gap, PageAction::Page(b)) =
                    (triple[0].action, triple[1].action, triple[2].action)
                {
                    assert!(b > a + 1, "an ellipsis must cover a skipped page");
                }
            }
        }
    }
}

#[test]
fn previous_is_disabled_on_the_first_page() {
    let layout = pagination_layout(row(80), state(0, 5, false), LABELS);
    let previous = &layout.controls[0];
    assert_eq!(previous.action, PageAction::Previous);
    assert!(!previous.enabled);
}

#[test]
fn next_is_disabled_on_the_last_page_without_more() {
    let layout = pagination_layout(row(80), state(4, 5, false), LABELS);
    let next = layout.controls.last().expect("next control");
    assert_eq!(next.action, PageAction::Next);
    assert!(!next.enabled);
}

#[test]
fn has_more_enables_next_on_the_last_page() {
    let layout = pagination_layout(row(80), state(4, 5, true), LABELS);
    let next = layout.controls.last().expect("next control");
    assert!(next.enabled);
}

#[test]
fn ellipses_are_absent_when_all_pages_fit_and_never_clickable() {
    let tight = pagination_layout(row(80), state(3, 7, false), LABELS);
    assert!(
        tight
            .controls
            .iter()
            .all(|control| control.action != PageAction::Gap)
    );

    let windowed = pagination_layout(row(80), state(24, 50, false), LABELS);
    let ellipses: Vec<_> = windowed
        .controls
        .iter()
        .filter(|control| control.action == PageAction::Gap)
        .collect();
    assert_eq!(ellipses.len(), 2);
    for ellipsis in ellipses {
        assert_eq!(ellipsis.label, "…");
        assert_eq!(
            ellipsis.rect.width, ELLIPSIS_WIDTH,
            "an ellipsis control occupies one column"
        );
        assert!(!ellipsis.enabled);
        assert!(!ellipsis.current);
    }
}

#[test]
fn rects_cover_their_drawn_labels_and_stay_inside_the_reserved_row() {
    let layout = pagination_layout(row(80), state(24, 50, false), LABELS);
    let mut buf = Buffer::empty(row(80));
    render_pagination(&mut buf, &layout, &dark_theme());
    let previous = &layout.controls[0];
    let next = layout.controls.last().expect("next control");
    let band: Vec<_> = layout
        .controls
        .iter()
        .filter(|control| matches!(control.action, PageAction::Page(_) | PageAction::Gap))
        .collect();
    for control in &layout.controls {
        assert_eq!(
            control.rect.width,
            label_width(&control.label),
            "rect width must match the label"
        );
        assert_eq!(rect_text(&buf, control.rect), control.label);
    }
    assert_eq!(previous.rect.x, 0);
    assert_eq!(band[0].rect.x, previous.rect.right() + GAP);
    for pair in band.windows(2) {
        assert_eq!(pair[1].rect.x, pair[0].rect.right() + GAP);
    }
    assert_eq!(next.rect.x, band.last().unwrap().rect.right() + GAP);
    assert_eq!(layout.width, next.rect.right());
}

#[test]
fn current_page_is_accent_on_selection_and_bold() {
    let theme = dark_theme();
    let semantic = &theme.semantic;
    let layout = pagination_layout(row(80), state(1, 50, false), LABELS);
    let mut buf = Buffer::empty(row(80));
    render_pagination(&mut buf, &layout, &theme);

    let current = layout
        .controls
        .iter()
        .find(|control| control.current)
        .expect("current page");
    assert_eq!(current.label, "2 ", "two-column page control");
    for x in current.rect.x..current.rect.right() {
        let style = buf[(x, current.rect.y)].style();
        assert_eq!(style.fg, Some(semantic.accent));
        assert_eq!(style.bg, Some(semantic.surface.selection));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    let other = layout
        .controls
        .iter()
        .find(|control| !control.current && control.action == PageAction::Page(3))
        .expect("last page");
    let style = buf[(other.rect.x, other.rect.y)].style();
    assert_eq!(style.fg, Some(semantic.text.secondary));
    assert_ne!(style.bg, Some(semantic.surface.selection));
    assert!(style.add_modifier.is_empty());

    let previous = buf[(layout.controls[0].rect.x, 0)].style();
    assert_eq!(previous.fg, Some(semantic.accent));
}

#[test]
fn disabled_controls_use_the_dim_text_token() {
    let theme = dark_theme();
    let layout = pagination_layout(row(80), state(0, 4, false), LABELS);
    let mut buf = Buffer::empty(row(80));
    render_pagination(&mut buf, &layout, &theme);
    for control in &layout.controls {
        if !control.enabled {
            let style = buf[(control.rect.x, control.rect.y)].style();
            assert_eq!(style.fg, Some(theme.semantic.text.dim));
        }
    }
}

#[test]
fn narrow_rows_fall_back_to_the_compact_form() {
    let layout = pagination_layout(row(21), state(4, 20, false), LABELS);
    assert_eq!(labels_of(&layout), vec!["‹ Prev", "  5/20 ", "Next ›"]);
    assert_eq!(layout.width, 21);
    let indicator = &layout.controls[1];
    assert!(indicator.current);
    assert!(!indicator.enabled);
    assert_eq!(indicator.action, PageAction::Page(4));
    assert!(
        layout.controls.last().unwrap().enabled,
        "the compact form keeps Next reachable"
    );
    assert_eq!(layout.controls.last().unwrap().action, PageAction::Next);
}

#[test]
fn rows_too_narrow_for_the_compact_form_are_clipped() {
    let area = row(10);
    let layout = pagination_layout(area, state(4, 20, false), LABELS);
    assert_eq!(layout.width, 10);
    assert_eq!(labels_of(&layout), vec!["‹ Prev", "  5/20 "]);
    for control in &layout.controls {
        assert!(control.rect.right() <= area.right());
    }
    let mut buf = Buffer::empty(area);
    render_pagination(&mut buf, &layout, &dark_theme());
    assert_eq!(rect_text(&buf, layout.controls[1].rect), "  …");
}

#[test]
fn empty_areas_produce_an_empty_layout() {
    for area in [
        Rect::new(0, 0, 0, 1),
        Rect::new(0, 0, 40, 0),
        Rect::new(3, 2, 0, 0),
    ] {
        let layout = pagination_layout(area, state(1, 5, true), LABELS);
        assert!(layout.controls.is_empty());
        assert_eq!(layout.width, 0);
    }
}

#[test]
fn zero_page_count_is_treated_as_one_page() {
    let layout = pagination_layout(row(40), state(0, 0, false), LABELS);
    assert_eq!(labels_of(&layout), vec!["‹ Prev", "1", "Next ›"]);
    assert!(!layout.controls[0].enabled);
    assert!(!layout.controls[2].enabled);
    assert_eq!(layout.controls[1].rect.width, 1);
}

#[test]
fn page_is_clamped_to_the_last_page() {
    let layout = pagination_layout(row(40), state(99, 3, false), LABELS);
    assert_eq!(state(99, 3, false).current(), 2);
    assert_eq!(labels_of(&layout), vec!["‹ Prev", "1", "2", "3", "Next ›"]);
    assert_eq!(pages_of(&layout), vec![0, 1, 2]);
    assert!(layout.controls[3].current);
}

#[test]
fn render_never_clears_cells_outside_the_controls() {
    let mut buf = Buffer::empty(row(30));
    buf.set_string(0, 0, "x".repeat(30), Style::default());
    let layout = pagination_layout(Rect::new(2, 0, 25, 1), state(0, 3, false), LABELS);
    render_pagination(&mut buf, &layout, &dark_theme());
    assert_eq!(buf[(0, 0)].symbol(), "x");
    assert_eq!(buf[(1, 0)].symbol(), "x");
    for control in &layout.controls {
        assert_eq!(rect_text(&buf, control.rect), control.label);
    }
    let last = layout.controls.last().unwrap().rect.right();
    for x in last..30 {
        assert_eq!(buf[(x, 0)].symbol(), "x");
    }
}

/// Width of the drawn page-number band, gaps included.
fn band_width(layout: &PaginationLayout) -> u16 {
    let band: Vec<_> = layout
        .controls
        .iter()
        .filter(|control| matches!(control.action, PageAction::Page(_) | PageAction::Gap))
        .collect();
    let mut width = 0u16;
    for (index, control) in band.iter().enumerate() {
        if index > 0 {
            width = width.saturating_add(GAP);
        }
        width = width.saturating_add(control.rect.width);
    }
    width
}

#[test]
fn reserved_band_covers_every_window() {
    for page_count in 1..=30usize {
        let reserved = reserved_band_width(page_count);
        assert!(reserved > 0, "page_count {page_count} reserves a band");
        for page in 0..page_count {
            let layout = pagination_layout(row(200), state(page, page_count, false), LABELS);
            let actual = band_width(&layout);
            assert!(
                actual <= reserved,
                "page {page}/{page_count}: window {actual} exceeds reserved {reserved}"
            );
        }
    }
}

#[test]
fn pager_columns_are_stable_while_paging() {
    for page_count in [1usize, 6, 7, 8, 9, 20, 50] {
        let digits = page_digits(page_count);
        let mut next_x = None;
        let mut previous_x = None;
        let mut row_width = None;
        for page in 0..page_count {
            let layout = pagination_layout(row(200), state(page, page_count, false), LABELS);
            let previous = &layout.controls[0];
            assert_eq!(previous.action, PageAction::Previous);
            let next = layout.controls.last().expect("next control");
            assert_eq!(next.action, PageAction::Next);
            assert!(
                layout
                    .controls
                    .iter()
                    .filter(|control| matches!(control.action, PageAction::Page(_)))
                    .all(|control| control.rect.width == digits),
                "page {page}/{page_count}: page controls must span {digits} columns"
            );

            match previous_x {
                Some(x) => assert_eq!(
                    previous.rect.x, x,
                    "Previous moved on page {page}/{page_count}"
                ),
                None => previous_x = Some(previous.rect.x),
            }
            match next_x {
                Some(x) => assert_eq!(next.rect.x, x, "Next moved on page {page}/{page_count}"),
                None => next_x = Some(next.rect.x),
            }
            match row_width {
                Some(width) => assert_eq!(
                    layout.width, width,
                    "row width changed on page {page}/{page_count}"
                ),
                None => row_width = Some(layout.width),
            }
        }
    }
}

#[test]
fn compact_indicator_keeps_a_constant_width() {
    let page_count = 50usize;
    let digits = page_digits(page_count);
    let mut next_x = None;
    for page in 0..page_count {
        let layout = pagination_layout(row(21), state(page, page_count, false), LABELS);
        assert_eq!(layout.controls.len(), 3, "compact row on page {page}");
        let indicator = &layout.controls[1];
        assert!(indicator.current);
        assert_eq!(
            label_width(&indicator.label),
            2 * digits + 3,
            "page {page}: compact indicator must keep a fixed width"
        );
        let next = layout.controls.last().unwrap();
        match next_x {
            Some(x) => assert_eq!(next.rect.x, x, "Next moved on compact page {page}"),
            None => next_x = Some(next.rect.x),
        }
    }
}
