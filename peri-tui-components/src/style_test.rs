//! `NO_COLOR` style stripping: colors out, modifiers/symbols/alignment stay.

use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[test]
fn removes_all_colors_keeps_modifiers_and_text() {
    let line = Line::from(vec![
        Span::styled(
            "◐ ",
            Style::default()
                .fg(Color::Rgb(125, 207, 255))
                .bg(Color::Rgb(10, 10, 10)),
        ),
        Span::styled(
            "Running",
            Style::default()
                .fg(Color::Rgb(255, 107, 128))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " 12s",
            Style::default()
                .fg(Color::Rgb(80, 80, 80))
                .underline_color(Color::Rgb(1, 2, 3)),
        ),
    ]);
    let stripped = super::strip_line_colors(&line);

    assert_eq!(stripped.spans.len(), 3, "span structure is preserved");
    for (original, stripped) in line.spans.iter().zip(stripped.spans.iter()) {
        assert_eq!(original.content, stripped.content, "text/symbols stay");
        assert_eq!(stripped.style.fg, None);
        assert_eq!(stripped.style.bg, None);
        assert_eq!(stripped.style.underline_color, None);
    }
    assert!(
        stripped.spans[1]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert!(
        !stripped.spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn keeps_unicode_symbols() {
    let line = Line::from(vec![
        Span::styled("✓", Style::default().fg(Color::Rgb(78, 186, 101))),
        Span::styled(" 完成", Style::default().fg(Color::Rgb(200, 200, 200))),
    ]);
    let stripped = super::strip_line_colors(&line);
    assert_eq!(stripped.spans[0].content, "✓");
    assert_eq!(stripped.spans[1].content, " 完成");
}

#[test]
fn preserves_alignment_and_line_style_modifiers() {
    let line = Line {
        spans: vec![Span::styled(
            "title",
            Style::default().fg(Color::Rgb(1, 2, 3)),
        )],
        alignment: Some(Alignment::Center),
        style: Style::default()
            .fg(Color::Rgb(9, 9, 9))
            .add_modifier(Modifier::ITALIC),
    };
    let stripped = super::strip_line_colors(&line);
    assert_eq!(stripped.alignment, Some(Alignment::Center));
    assert_eq!(stripped.style.fg, None);
    assert_eq!(stripped.style.bg, None);
    assert!(stripped.style.add_modifier.contains(Modifier::ITALIC));
}

#[test]
fn plain_style_is_unchanged() {
    let line = Line::from(vec![Span::raw("plain")]);
    let stripped = super::strip_line_colors(&line);
    assert_eq!(stripped.spans[0].content, "plain");
    assert_eq!(stripped.spans[0].style, Style::default());
}

#[test]
fn sub_modifier_survives() {
    let style = Style {
        sub_modifier: Modifier::BOLD,
        ..Style::default().fg(Color::Red)
    };
    let stripped = super::strip_style_color(style);
    assert_eq!(stripped.fg, None);
    assert!(stripped.sub_modifier.contains(Modifier::BOLD));
}
