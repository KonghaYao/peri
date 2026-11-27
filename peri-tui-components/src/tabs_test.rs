use super::*;

fn rects(area: Rect, titles: &[&str], selected: usize) -> Vec<Rect> {
    tab_layout(area, titles, selected).rects
}

#[test]
fn layout_matches_tabs_padding_and_divider() {
    let area = Rect::new(0, 0, 40, 1);
    let got = rects(area, &["Timeline", "Turns", "Info"], 0);
    assert_eq!(
        got,
        vec![
            Rect::new(1, 0, 8, 1),
            Rect::new(14, 0, 5, 1),
            Rect::new(24, 0, 4, 1),
        ]
    );
    // Adjacent labels keep the documented five-column gap.
    assert_eq!(got[1].x - got[0].right(), 5);
    assert_eq!(got[2].x - got[1].right(), 5);
}

#[test]
fn selected_index_does_not_change_geometry() {
    let area = Rect::new(4, 2, 40, 1);
    let first = rects(area, &["Timeline", "Turns", "Info"], 0);
    let last = rects(area, &["Timeline", "Turns", "Info"], 2);
    assert_eq!(first, last);
    assert_eq!(first[0], Rect::new(5, 2, 8, 1));
}

#[test]
fn cjk_titles_use_display_width() {
    let area = Rect::new(0, 0, 40, 1);
    let got = rects(area, &["时间线", "轮次"], 0);
    assert_eq!(got, vec![Rect::new(1, 0, 6, 1), Rect::new(12, 0, 4, 1)]);
}

#[test]
fn empty_or_zero_width_area_has_no_regions() {
    assert!(rects(Rect::new(0, 0, 0, 1), &["A", "B"], 0).is_empty());
    assert!(rects(Rect::new(0, 0, 10, 0), &["A", "B"], 0).is_empty());
    assert!(rects(Rect::new(0, 0, 10, 1), &[], 0).is_empty());
}

#[test]
fn titles_that_do_not_fit_are_clipped_safely() {
    let area = Rect::new(0, 0, 10, 1);
    let got = rects(area, &["Timeline", "Turns"], 0);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0], Rect::new(1, 0, 8, 1));
    for rect in got {
        assert!(rect.right() <= area.right());
    }

    let narrow = rects(Rect::new(0, 0, 6, 1), &["Timeline", "Turns"], 0);
    assert_eq!(narrow, vec![Rect::new(1, 0, 5, 1)]);
    assert!(narrow[0].right() <= 6);
}
