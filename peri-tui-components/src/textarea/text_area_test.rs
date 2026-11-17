use super::*;

#[test]
fn explicit_base_style_controls_cursor_line_and_padding() {
    let mut state = TextAreaState::default();
    state.insert_str("ab");
    let area = Rect::new(0, 0, 8, 2);
    let mut buffer = Buffer::empty(area);
    let base = Style::default().fg(Color::Green).bg(Color::Reset);
    let cursor = Style::default().bg(Color::Blue);
    TextArea::new(cursor, Style::default())
        .base_style(base)
        .render_ref(area, &mut buffer, &mut state);

    assert_eq!(buffer[(0, 0)].fg, Color::Green);
    assert_eq!(buffer[(0, 0)].bg, Color::Reset);
    assert_eq!(buffer[(2, 0)].bg, Color::Blue);
    assert_eq!(buffer[(5, 0)].fg, Color::Green);
    assert_eq!(buffer[(5, 0)].bg, Color::Reset);
}

#[test]
fn width_reserve_changes_wrapping_without_moving_origin() {
    let mut state = TextAreaState::default();
    state.insert_str("abcde");
    let area = Rect::new(2, 1, 5, 3);
    let mut original = Buffer::empty(Rect::new(0, 0, 8, 5));
    let mut full_width = original.clone();
    TextArea::new(Style::default(), Style::default())
        .show_cursor(false)
        .render_ref(area, &mut original, &mut state);
    TextArea::new(Style::default(), Style::default())
        .width_reserve(0)
        .show_cursor(false)
        .render_ref(area, &mut full_width, &mut state);

    assert_eq!(original[(2, 1)].symbol(), "a");
    assert_eq!(original[(2, 2)].symbol(), "d");
    assert_eq!(full_width[(2, 1)].symbol(), "a");
    assert_eq!(full_width[(6, 1)].symbol(), "e");
    assert_eq!(full_width[(2, 2)].symbol(), " ");
}

#[test]
fn width_reserve_larger_than_area_keeps_a_one_cell_viewport() {
    let mut state = TextAreaState::default();
    state.insert_str("abc");
    let area = Rect::new(0, 0, 2, 4);
    let mut buffer = Buffer::empty(area);
    TextArea::new(Style::default(), Style::default())
        .width_reserve(u16::MAX)
        .show_cursor(false)
        .render_ref(area, &mut buffer, &mut state);
    assert_eq!(buffer[(0, 0)].symbol(), "a");
    assert_eq!(buffer[(0, 1)].symbol(), "b");
    assert_eq!(buffer[(0, 2)].symbol(), "c");
}
