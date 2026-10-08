use super::*;
use ratatui_kit::test_util::render_frame;

fn row_text(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
    (0..buffer.area.width)
        .map(|column| buffer[(column, row)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn viewport_starts_inside_wrapped_logical_line() {
    let buffer = render_frame(
        render_viewport(vec![Line::from("first second third")], 1),
        7,
        2,
    );
    assert_eq!(row_text(&buffer, 0), "second");
    assert_eq!(row_text(&buffer, 1), "third");
}

#[test]
fn viewport_scrolls_inside_footer_without_repeating_hidden_rows() {
    let buffer = render_frame(
        render_viewport(
            vec![Line::from(""), Line::from(""), Line::from("status")],
            2,
        ),
        20,
        2,
    );
    assert_eq!(row_text(&buffer, 0), "status");
    assert_eq!(row_text(&buffer, 1), "");
}
