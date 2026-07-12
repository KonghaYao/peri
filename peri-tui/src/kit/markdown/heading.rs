use pulldown_cmark::HeadingLevel;
use ratatui::text::Line;
use ratatui_kit_markdown::MarkdownTheme;

use super::span_style::apply_span_styles;

pub(crate) fn heading_line(
    _level: &HeadingLevel,
    line: &Line<'static>,
    theme: &MarkdownTheme,
) -> Line<'static> {
    // 不渲染 # 前缀和标题样式，当普通段落处理
    Line::from(apply_span_styles(&line.spans, theme, None))
}
