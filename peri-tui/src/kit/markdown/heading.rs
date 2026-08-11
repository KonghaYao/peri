use peri_theme::atoms::THEME_ATOM;
use pulldown_cmark::HeadingLevel;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui_kit_markdown::MarkdownTheme;

use super::span_style::apply_span_styles;

pub(crate) fn heading_line(
    _level: &HeadingLevel,
    line: &Line<'static>,
    theme: &MarkdownTheme,
) -> Line<'static> {
    // [Slice 3] 标题去高饱和彩虹色（§6.2：heading 主要依靠 bold 与空行）——
    // 覆盖 kit 默认的 palette.warning 黄色，改用 text.primary + BOLD。
    let sem = THEME_ATOM.state().read().semantic;
    let heading_style = Style::default()
        .fg(sem.text.primary)
        .add_modifier(Modifier::BOLD);
    // 不渲染 # 前缀，但应用标题样式
    Line::from(apply_span_styles(&line.spans, theme, Some(heading_style)))
}
