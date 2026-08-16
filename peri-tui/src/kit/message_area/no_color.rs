/// NO_COLOR 剥离 pass（§12）：剥离可见行的前景/背景/下划线色，**保留 modifier**
/// （bold/italic/dim 等）与符号、文本——任何状态都不能只依赖颜色。
///
/// [G3] 只作用于视口裁剪后的可见行（视口行数 ≈ 终端高度），不触碰渲染缓存，
/// 不写业务状态；颜色剥离后的行仅本帧使用。
pub(super) fn strip_line_colors(
    line: &ratatui_kit::ratatui::text::Line<'static>,
) -> ratatui_kit::ratatui::text::Line<'static> {
    // Line 级 style 与 span 级 style 同等处理：剥离颜色、保留 modifier。
    let line_style = strip_style_color(line.style);
    let spans = line
        .spans
        .iter()
        .map(|span| {
            ratatui_kit::ratatui::text::Span::styled(
                span.content.clone(),
                strip_style_color(span.style),
            )
        })
        .collect();
    ratatui_kit::ratatui::text::Line {
        spans,
        alignment: line.alignment,
        style: line_style,
    }
}

/// 剥离单个 Style 的颜色字段（fg/bg/underline_color），保留 modifier。
fn strip_style_color(s: ratatui_kit::ratatui::style::Style) -> ratatui_kit::ratatui::style::Style {
    ratatui_kit::ratatui::style::Style {
        fg: None,
        bg: None,
        underline_color: None,
        add_modifier: s.add_modifier,
        sub_modifier: s.sub_modifier,
    }
}
