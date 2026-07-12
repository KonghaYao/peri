use ratatui::{
    style::{Modifier, Style},
    text::Span,
};
use ratatui_kit_markdown::MarkdownTheme;

/// 对 span 列表逐个应用语义样式 + 清理哨兵修饰符。
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
            // 剥离 parser 内部哨兵修饰符：
            // - REVERSED（LINK_URL_MARKER）— 否则终端会反转前景/背景色
            // - DIM（INLINE_CODE_MARKER）— 否则文本会变暗
            carried_style.add_modifier.remove(Modifier::REVERSED);
            carried_style.add_modifier.remove(Modifier::DIM);
            let style = span_semantic_style(span, theme)
                .or(base_style)
                .unwrap_or_default()
                .patch(carried_style);
            Span::styled(content, style)
        })
        .collect()
}

/// 判断 span 语义类型（行内代码/链接/URL）并返回对应样式。
fn span_semantic_style(span: &Span<'static>, theme: &MarkdownTheme) -> Option<Style> {
    if span.style.add_modifier.contains(Modifier::REVERSED) {
        // LINK_URL_MARKER 哨兵 → link_url_style
        Some(theme.link_url_style)
    } else if span.style.add_modifier.contains(Modifier::UNDERLINED) {
        Some(theme.link_style)
    } else if span.style.add_modifier.contains(Modifier::DIM) {
        // INLINE_CODE_MARKER 哨兵 → inline_code_style
        Some(theme.inline_code_style)
    } else {
        None
    }
}
