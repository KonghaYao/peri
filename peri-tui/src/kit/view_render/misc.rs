//! 折叠组 / 分隔线渲染。

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::kit::tui_render_unit::{TuiCollapsedGroup, TuiDivider};
use peri_theme::atoms::THEME_ATOM;

pub(crate) fn render_collapsed_group(data: &TuiCollapsedGroup) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    vec![Line::from(vec![
        Span::styled("\u{25cf} ", Style::default().fg(semantic.status.success)),
        Span::styled(
            format!("{}\u{ff08}{}\u{9879}\u{ff09}", data.title, data.count),
            Style::default().fg(semantic.text.muted),
        ),
    ])]
}

pub(crate) fn render_divider(data: &TuiDivider) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    if let Some(ref label) = data.label {
        vec![Line::from(vec![
            Span::styled("\u{2500}\u{2500} ", Style::default().fg(semantic.text.dim)),
            Span::styled(label.clone(), Style::default().fg(semantic.text.muted)),
            Span::styled(" \u{2500}\u{2500}", Style::default().fg(semantic.text.dim)),
        ])]
    } else {
        vec![Line::from(vec![Span::styled(
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            Style::default().fg(semantic.text.dim),
        )])]
    }
}
