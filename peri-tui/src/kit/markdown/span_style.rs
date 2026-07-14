use std::borrow::Cow;

use ratatui::{
    style::{Modifier, Style},
    text::Span,
};
use ratatui_kit_markdown::MarkdownTheme;

/// 检测 span 内容是否被 backtick 包裹（parser 产出的行内代码 span）。
fn is_inline_code_span(text: &str) -> bool {
    text.len() >= 2 && text.starts_with('`') && text.ends_with('`')
}

/// 对 span 列表逐个应用语义样式 + 清理哨兵修饰符。
pub(crate) fn apply_span_styles(
    spans: &[Span<'static>],
    theme: &MarkdownTheme,
    base_style: Option<Style>,
) -> Vec<Span<'static>> {
    spans
        .iter()
        .map(|span| {
            let raw_content: &str = span.content.as_ref();
            let mut carried_style = span.style;
            // 剥离 parser 内部哨兵修饰符：
            // - REVERSED（LINK_URL_MARKER）— 否则终端会反转前景/背景色
            carried_style.add_modifier.remove(Modifier::REVERSED);
            carried_style.add_modifier.remove(Modifier::DIM);
            let style = span_semantic_style(span, theme)
                .or(base_style)
                .unwrap_or_default()
                .patch(carried_style);
            // 行内代码：去掉前后的 backtick 包裹符号
            let content: Cow<'static, str> = if is_inline_code_span(raw_content) {
                Cow::Owned(raw_content[1..raw_content.len() - 1].to_string())
            } else {
                span.content.clone()
            };
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
    } else if is_inline_code_span(span.content.as_ref()) {
        // ratatui-kit-markdown 0.3.0 parser 对行内代码使用 Span::raw() 不带修饰符，
        // 但内容被包装为 `code` 格式。用 backtick 包裹特征做文本级检测。
        // bg=None：行内代码无背景色，仅前景色区分。
        let mut style = theme.inline_code_style;
        style.bg = None;
        Some(style)
    } else {
        None
    }
}
