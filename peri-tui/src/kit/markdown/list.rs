use ratatui::{
    style::Style,
    text::{Line, Span},
};
use ratatui_kit_markdown::{ListItemData, MarkdownTheme};

use super::span_style::apply_span_styles;

pub(crate) fn list_item_line(
    item: &ListItemData,
    theme: &MarkdownTheme,
    base_style: Style,
) -> Line<'static> {
    let indent = "  ".repeat(item.depth as usize);
    let prefix = if item.ordered {
        format!("{}{}. ", indent, item.number.unwrap_or(1))
    } else {
        format!("{indent}• ")
    };
    let mut spans = vec![Span::styled(prefix, theme.list_marker_style)];
    spans.extend(apply_span_styles(&item.spans, theme, Some(base_style)));
    Line::from(spans)
}
