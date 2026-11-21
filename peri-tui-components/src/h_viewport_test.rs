use super::{HorizontalViewport, content_width};

#[test]
fn fitting_content_has_no_viewport_bar() {
    let viewport = HorizontalViewport::new(40, 40);
    assert!(!viewport.overflows());
    assert_eq!(viewport.max_offset(), 0);
    assert_eq!(viewport.clamp(7), 0);
    assert_eq!(viewport.pan(7, 8), 0);
    assert_eq!(viewport.bar(40, 0), None);
}

#[test]
fn overflow_exposes_hidden_columns_and_clamps_both_ends() {
    let viewport = HorizontalViewport::new(40, 100);
    assert!(viewport.overflows());
    assert_eq!(viewport.viewport(), 40);
    assert_eq!(viewport.content(), 100);
    assert_eq!(viewport.max_offset(), 60);
    assert_eq!(viewport.clamp(200), 60);
    assert_eq!(viewport.pan(56, 8), 60);
    assert_eq!(viewport.pan(4, -8), 0);
    let bar = viewport.bar(40, 30).expect("overflowing row has a bar");
    assert_eq!(bar.track, 38);
    assert!(bar.thumb_len >= 1 && bar.thumb_len <= bar.track);
}

#[test]
fn narrow_rows_still_pan_without_a_bar() {
    let viewport = HorizontalViewport::new(40, 100);
    assert_eq!(
        viewport.bar(2, 0),
        None,
        "two cells cannot host arrows + track"
    );
    assert_eq!(viewport.bar(0, 0), None);
}

#[test]
fn content_width_counts_terminal_cells_not_chars() {
    let lines = vec!["abc".to_string(), "你a".to_string()];
    assert_eq!(content_width(&lines), 3);
    assert_eq!(content_width(&[]), 0);
}
