//! 用户消息气泡渲染。

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::kit::markdown::MarkdownSegment;
use crate::kit::tui_render_unit::ReminderInfo;
use peri_theme::atoms::THEME_ATOM;

/// 渲染用户消息气泡（含 markdown 解析）。
pub(crate) fn render_user_bubble(
    text: &str,
    width: usize,
    reminder: Option<&ReminderInfo>,
) -> Vec<MarkdownSegment> {
    if let Some(info) = reminder {
        return vec![MarkdownSegment::Text(render_reminder_condensed(info))];
    }

    let semantic = THEME_ATOM.state().read().semantic;
    let component = THEME_ATOM.state().read().component;
    let user_bg = component.message.user_bg;
    let palette_state = peri_theme::atoms::PALETTE_ATOM.state();
    let palette_guard = palette_state.read();
    let segments = crate::kit::markdown::parse_markdown(text, width, *palette_guard);

    let mut out: Vec<MarkdownSegment> = Vec::with_capacity(segments.len() + 1);
    out.push(MarkdownSegment::Text(vec![Line::from("")]));

    for seg in segments {
        match seg {
            MarkdownSegment::Text(mut lines) => {
                let mut wrapped: Vec<Line<'static>> = Vec::with_capacity(lines.len());
                for (i, line) in lines.drain(..).enumerate() {
                    if i == 0 {
                        let mut spans = vec![Span::styled(
                            "\u{276f} ",
                            Style::default()
                                .fg(semantic.accent)
                                .add_modifier(Modifier::BOLD)
                                .bg(user_bg),
                        )];
                        for span in line.spans {
                            spans.push(span.clone().patch_style(Style::default().bg(user_bg)));
                        }
                        wrapped.push(Line::from(spans));
                    } else {
                        let mut spans = vec![Span::styled("  ", Style::default().bg(user_bg))];
                        for span in line.spans {
                            spans.push(span.clone().patch_style(Style::default().bg(user_bg)));
                        }
                        wrapped.push(Line::from(spans));
                    }
                }
                out.push(MarkdownSegment::Text(wrapped));
            }
            MarkdownSegment::Table(table) => {
                out.push(MarkdownSegment::Table(table));
            }
        }
    }
    out
}

/// 两行缩略渲染 system-reminder：L1 类型标签（dim italic），L2 数据摘要（⎿ muted）。
fn render_reminder_condensed(info: &ReminderInfo) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let mut lines = vec![Line::from(Span::styled(
        info.reminder_type.label(),
        Style::default()
            .fg(semantic.text.dim)
            .add_modifier(Modifier::ITALIC),
    ))];
    if !info.summary.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  \u{23bf} ", Style::default().fg(semantic.text.dim)),
            Span::styled(
                info.summary.clone(),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }
    lines
}
