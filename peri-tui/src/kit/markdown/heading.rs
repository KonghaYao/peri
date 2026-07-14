use pulldown_cmark::HeadingLevel;
use ratatui::text::Line;
use ratatui_kit_markdown::MarkdownTheme;

use super::span_style::apply_span_styles;

pub(crate) fn heading_line(
    _level: &HeadingLevel,
    line: &Line<'static>,
    theme: &MarkdownTheme,
) -> Line<'static> {
    // 不渲染 # 前缀，但应用标题样式（黄色 + BOLD）
    Line::from(apply_span_styles(
        &line.spans,
        theme,
        Some(theme.heading_style),
    ))
}
