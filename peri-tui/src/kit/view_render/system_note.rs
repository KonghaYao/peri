//! 系统提示/状态信息渲染。

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use peri_theme::atoms::THEME_ATOM;

pub(crate) fn render_system_note(
    data: &crate::kit::tui_render_unit::TuiSystemNote,
) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for line_text in data.text.lines() {
        let (prefix_str, color) = if line_text.starts_with('\u{273b}') {
            // ✻ 元信息前缀 → dim 色
            ("\u{273b} ", semantic.text.dim)
        } else if line_text.starts_with("\u{23bf}") {
            // 行首 ⎿（无缩进）→ muted 色
            ("\u{23bf} ", semantic.text.muted)
        } else if line_text.starts_with("  \u{23bf}") {
            // 缩进 ⎿ → error 色
            ("  \u{23bf} ", semantic.status.error)
        } else if line_text.contains('\u{274c}')
            || line_text.contains("\u{5931}\u{8d25}")
            || line_text.contains("error")
        {
            // 含 ❌/失败/error 关键词 → error 色，无前缀
            ("", semantic.status.error)
        } else if line_text.contains("warning") || line_text.contains("warn") {
            // 含 warning/warn 关键词 → warning 色，无前缀
            ("", semantic.status.warning)
        } else {
            // 其余行 → muted 色，无前缀
            ("", semantic.text.muted)
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        // 跳过已消费的前缀字符
        let content_text = if prefix_str.contains('\u{273b}') {
            spans.push(Span::styled(
                "\u{273b} ".to_string(),
                Style::default().fg(semantic.text.dim),
            ));
            line_text
                .strip_prefix('\u{273b}')
                .unwrap_or(line_text)
                .trim_start()
        } else if prefix_str.contains("\u{23bf}") && prefix_str.starts_with("  ") {
            spans.push(Span::styled(
                "  \u{23bf} ".to_string(),
                Style::default().fg(semantic.text.dim),
            ));
            line_text
                .strip_prefix("  \u{23bf}")
                .unwrap_or(line_text)
                .trim_start()
        } else if prefix_str.contains("\u{23bf}") {
            spans.push(Span::styled(
                "\u{23bf} ".to_string(),
                Style::default().fg(semantic.text.dim),
            ));
            line_text
                .strip_prefix("\u{23bf}")
                .unwrap_or(line_text)
                .trim_start()
        } else {
            line_text
        };
        if !content_text.is_empty() {
            spans.push(Span::styled(
                content_text.to_string(),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}
