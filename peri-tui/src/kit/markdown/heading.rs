use pulldown_cmark::HeadingLevel;
use ratatui::text::{Line, Span};
use ratatui_kit_markdown::MarkdownTheme;

use super::span_style::apply_span_styles;

pub(crate) fn heading_level_num(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

pub(crate) fn heading_line(
    level: &HeadingLevel,
    line: &Line<'static>,
    theme: &MarkdownTheme,
) -> Line<'static> {
    let level_num = heading_level_num(*level);
    let prefix = "#".repeat(level_num);
    let mut spans = vec![
        Span::styled(prefix, theme.heading_marker_style),
        Span::raw(" "),
    ];
    spans.extend(apply_span_styles(
        &line.spans,
        theme,
        Some(theme.heading_style),
    ));
    Line::from(spans)
}
