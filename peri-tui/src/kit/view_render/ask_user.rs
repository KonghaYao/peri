//! AskUser 回答块渲染。

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::i18n;
use crate::kit::tui_render_unit::TuiAskUserBlock;
use peri_theme::atoms::THEME_ATOM;

pub(crate) fn render_ask_user_block(data: &TuiAskUserBlock) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let mut lines: Vec<Line<'static>> = Vec::new();

    let title_color = if data.is_error {
        semantic.status.error
    } else {
        semantic.status.success
    };
    lines.push(Line::from(Span::styled(
        i18n::tr("render-user-answered"),
        Style::default().fg(title_color),
    )));

    for item in &data.items {
        let prefix = Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim));
        let item_color = if data.is_error {
            semantic.status.error
        } else {
            semantic.text.muted
        };
        let content = Span::styled(
            format!("{} \u{2192} {}", item.header, item.answer),
            Style::default().fg(item_color),
        );
        lines.push(Line::from(vec![prefix, content]));
    }

    lines
}
