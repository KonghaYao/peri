use ratatui::{
    style::{Modifier, Style},
    text::Span,
};
use ratatui_kit_markdown::MarkdownTheme;

/// 对 span 列表逐个应用语义样式 + 清理 `REVERSED` 哨兵。
pub(crate) fn apply_span_styles(
    spans: &[Span<'static>],
    theme: &MarkdownTheme,
    base_style: Option<Style>,
) -> Vec<Span<'static>> {
    spans
        .iter()
        .map(|span| {
            let content = span.content.clone();
            let mut carried_style = span.style;
            // 剥离 parser 内部的 LINK_URL_MARKER 哨兵（Modifier::REVERSED），
            // 否则终端实际渲染会反转前景/背景色。
            carried_style.add_modifier.remove(Modifier::REVERSED);
            let style = span_semantic_style(span, theme)
                .or(base_style)
                .unwrap_or_default()
                .patch(carried_style);
            Span::styled(content, style)
        })
        .collect()
}

/// 判断 span 语义类型（链接/行内代码/URL）并返回对应样式。
fn span_semantic_style(span: &Span<'static>, theme: &MarkdownTheme) -> Option<Style> {
    let text = span.content.as_ref();
    if span.style.add_modifier.contains(Modifier::REVERSED) {
        // LINK_URL_MARKER 哨兵 → link_url_style
        Some(theme.link_url_style)
    } else if text.len() >= 2 && text.starts_with('`') && text.ends_with('`') {
        Some(theme.inline_code_style)
    } else if span.style.add_modifier.contains(Modifier::UNDERLINED) {
        Some(theme.link_style)
    } else {
        None
    }
}
