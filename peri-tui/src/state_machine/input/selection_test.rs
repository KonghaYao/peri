use super::selection::Selection;

#[test]
fn test_selection_normal_anchor_le_cursor() {
    let s = Selection::normal(0, 0, 0, 5);
    assert_eq!(s.start(), (0, 0));
    assert_eq!(s.end(), (0, 5));
}

#[test]
fn test_selection_normalizes_reversed_anchor() {
    // anchor (2,5) cursor (1,1)
    let s = Selection::normal(2, 5, 1, 1);
    assert_eq!(s.start(), (1, 1));
    assert_eq!(s.end(), (2, 5));
}

#[test]
fn test_selection_empty_when_anchor_eq_cursor() {
    let s = Selection::normal(0, 3, 0, 3);
    assert!(s.is_empty());
}

#[test]
fn test_selection_line_range_across_rows() {
    let s = Selection::normal(0, 10, 2, 5);
    let range = s.range();
    assert!(range.contains_row(0));
    assert!(range.contains_row(1));
    assert!(range.contains_row(2));
    assert!(!range.contains_row(3));
}
