use super::*;

#[test]
fn test_start_drag_sets_coords() {
    let mut ts = TextSelection::new();
    ts.start_drag(5, 10);
    assert_eq!(ts.start, Some((5, 10)));
    assert_eq!(ts.end, Some((5, 10)));
    assert!(ts.dragging);
    assert!(ts.selected_text.is_none());
}

#[test]
fn test_update_drag_moves_end() {
    let mut ts = TextSelection::new();
    ts.start_drag(0, 0);
    ts.update_drag(3, 8);
    assert_eq!(ts.start, Some((0, 0)));
    assert_eq!(ts.end, Some((3, 8)));
}

#[test]
fn test_end_drag_stops_dragging() {
    let mut ts = TextSelection::new();
    ts.start_drag(1, 2);
    ts.end_drag();
    assert!(!ts.dragging);
    assert_eq!(ts.start, Some((1, 2)));
    assert_eq!(ts.end, Some((1, 2)));
}

#[test]
fn test_clear_resets_all() {
    let mut ts = TextSelection::new();
    ts.start_drag(5, 10);
    ts.update_drag(8, 20);
    ts.end_drag();
    ts.set_selected_text(Some("hello".into()));
    ts.clear();
    assert!(ts.start.is_none());
    assert!(ts.end.is_none());
    assert!(!ts.dragging);
    assert!(ts.selected_text.is_none());
}

#[test]
fn test_is_active() {
    let mut ts = TextSelection::new();
    assert!(!ts.is_active());
    ts.start_drag(0, 0);
    assert!(ts.is_active());
    ts.end_drag();
    assert!(!ts.is_active());
    ts.set_selected_text(Some("x".into()));
    assert!(ts.is_active());
}

// --- Task 3: 坐标映射和文本提取测试 ---

fn make_wrap_map_entry(
    line_idx: usize,
    start: u16,
    end: u16,
    text: &str,
) -> crate::app::message_state::WrappedLineInfo {
    let char_widths: Vec<u8> = text
        .chars()
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) as u8)
        .collect();
    crate::app::message_state::WrappedLineInfo {
        line_idx,
        visual_row_start: start,
        visual_row_end: end,
        plain_text: text.to_string(),
        char_widths,
    }
}

#[test]
fn test_visual_to_logical_basic() {
    let wrap_map = vec![
        make_wrap_map_entry(0, 0, 1, "Hello"),
        make_wrap_map_entry(1, 1, 2, "World"),
    ];
    assert_eq!(visual_to_logical(0, 0, &wrap_map, 80), Some((0, 0)));
    assert_eq!(visual_to_logical(1, 0, &wrap_map, 80), Some((1, 0)));
}

#[test]
fn test_visual_to_logical_out_of_range() {
    let wrap_map = vec![make_wrap_map_entry(0, 0, 1, "Hello")];
    assert_eq!(visual_to_logical(99, 0, &wrap_map, 80), None);
}

#[test]
fn test_extract_selected_text_single_line() {
    let wrap_map = vec![make_wrap_map_entry(0, 0, 1, "Hello World")];
    let result = extract_selected_text((0, 2), (0, 8), &wrap_map, 80);
    // char 2..8 of "Hello World" = "llo Wo"
    assert_eq!(result, Some("llo Wo".to_string()));
}

#[test]
fn test_extract_selected_text_multi_line() {
    let wrap_map = vec![
        make_wrap_map_entry(0, 0, 1, "Line0"),
        make_wrap_map_entry(1, 1, 2, "Line1"),
        make_wrap_map_entry(2, 2, 3, "Line2"),
    ];
    let result = extract_selected_text((0, 0), (2, 5), &wrap_map, 80);
    assert_eq!(result, Some("Line0\nLine1\nLine2".to_string()));
}

#[test]
fn test_extract_selected_text_swapped() {
    let wrap_map = vec![
        make_wrap_map_entry(0, 0, 1, "Line0"),
        make_wrap_map_entry(1, 1, 2, "Line1"),
        make_wrap_map_entry(2, 2, 3, "Line2"),
    ];
    let result = extract_selected_text((2, 5), (0, 0), &wrap_map, 80);
    assert_eq!(result, Some("Line0\nLine1\nLine2".to_string()));
}

#[test]
fn test_extract_selected_text_partial_first_and_last() {
    let wrap_map = vec![
        make_wrap_map_entry(0, 0, 1, "Hello"),
        make_wrap_map_entry(1, 1, 2, "World"),
    ];
    let result = extract_selected_text((0, 2), (1, 3), &wrap_map, 80);
    assert_eq!(result, Some("llo\nWor".to_string()));
}

#[test]
fn test_char_col_to_offset_ascii() {
    let char_widths = vec![1, 1, 1, 1, 1, 1, 1, 1, 1, 1]; // "ABCDEFGHIJ"
    let offset = char_col_to_offset(&char_widths, 5, 0, 80);
    assert_eq!(offset, 5);
}

#[test]
fn test_char_col_to_offset_cjk() {
    let char_widths = vec![2, 2, 2, 2]; // "你好世界"
    let offset = char_col_to_offset(&char_widths, 4, 0, 80);
    assert_eq!(offset, 2);
}

// --- PanelTextSelection tests ---

#[test]
fn test_panel_selection_lifecycle() {
    let mut ps = PanelTextSelection::new();
    assert!(!ps.is_active());
    ps.start_drag(2, 5);
    assert!(ps.is_active());
    assert_eq!(ps.start, Some((2, 5)));
    assert_eq!(ps.end, Some((2, 5)));
    ps.update_drag(4, 10);
    assert_eq!(ps.end, Some((4, 10)));
    ps.end_drag();
    assert!(!ps.dragging);
    ps.set_selected_text(Some("test".into()));
    assert!(ps.is_active());
    ps.clear();
    assert!(!ps.is_active());
}

// --- extract_panel_text tests ---

#[test]
fn test_extract_panel_text_single_line() {
    let lines = vec!["Hello World".to_string()];
    let result = extract_panel_text((0, 2), (0, 8), &lines);
    assert_eq!(result, Some("llo Wo".to_string()));
}

#[test]
fn test_extract_panel_text_multi_line() {
    let lines = vec![
        "Line0".to_string(),
        "Line1".to_string(),
        "Line2".to_string(),
    ];
    let result = extract_panel_text((0, 0), (2, 5), &lines);
    assert_eq!(result, Some("Line0\nLine1\nLine2".to_string()));
}

#[test]
fn test_extract_panel_text_swapped() {
    let lines = vec![
        "Line0".to_string(),
        "Line1".to_string(),
        "Line2".to_string(),
    ];
    let result = extract_panel_text((2, 5), (0, 0), &lines);
    assert_eq!(result, Some("Line0\nLine1\nLine2".to_string()));
}

#[test]
fn test_extract_panel_text_partial() {
    let lines = vec!["Hello".to_string(), "World".to_string()];
    let result = extract_panel_text((0, 2), (1, 3), &lines);
    assert_eq!(result, Some("llo\nWor".to_string()));
}

#[test]
fn test_extract_panel_text_out_of_range() {
    let lines = vec!["Hello".to_string()];
    let result = extract_panel_text((5, 0), (5, 3), &lines);
    assert_eq!(result, None);
}
