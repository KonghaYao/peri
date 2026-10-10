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
        render_viewport(vec![Line::from("first second third")], 1, None),
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
            None,
        ),
        20,
        2,
    );
    assert_eq!(row_text(&buffer, 0), "status");
    assert_eq!(row_text(&buffer, 1), "");
}

/// [回归测试] 正文从折行内部开始时，指示器仍固定在视口末行；
/// 不能按逻辑行数计算位置或随 Paragraph 的 scroll 被裁掉。
#[test]
fn test_new_output_indicator_stays_at_viewport_bottom() {
    let buffer = render_frame(
        render_viewport(
            vec![Line::from("first second third fourth")],
            1,
            Some(Line::from("new")),
        ),
        7,
        3,
    );
    assert_eq!(row_text(&buffer, 0), "second");
    assert_eq!(row_text(&buffer, 1), "third");
    assert_eq!(row_text(&buffer, 2), "new");
}
