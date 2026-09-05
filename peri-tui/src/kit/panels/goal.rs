//! 当前 session Goal 的只读详情面板。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{GOAL_SNAPSHOT, LANG_VERSION};
use peri_acp_types::goal::GoalStatus;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
pub fn GoalPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang = hooks.use_atom(&LANG_VERSION);
    let goal_store = hooks.use_atom(&GOAL_SNAPSHOT);
    let goal = goal_store.read().clone();
    let sv = hooks.use_state(ScrollViewState::default);

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        if key.code == KeyCode::Esc {
            close_panel();
        }
        EventResult::Consumed
    });

    let theme = theme_def.read();
    let title = Style::new().fg(theme.component.panel.title).bold();
    let label = Style::new().fg(theme.semantic.text.muted);
    let value = Style::new().fg(theme.semantic.text.primary);
    let mut lines = Vec::new();

    if let Some(goal) = goal {
        lines.push(Line::from(Span::styled(
            goal.objective.unwrap_or_else(|| i18n::tr("ui-empty")),
            title,
        )));
        lines.push(Line::from(""));
        lines.push(detail_line(
            "goal-detail-status",
            status_label(goal.status),
            label,
            value,
        ));
        lines.push(detail_line(
            "goal-detail-continuations",
            goal.continuation_count.to_string(),
            label,
            value,
        ));
        if let Some(reason) = goal
            .blocked_reason
            .filter(|reason| !reason.trim().is_empty())
        {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                i18n::tr("goal-detail-blocked-reason"),
                label,
            )));
            lines.push(Line::from(Span::styled(reason, value)));
        }
    } else {
        lines.push(Line::from(Span::styled(
            i18n::tr("goal-detail-empty"),
            label,
        )));
    }
    drop(theme);

    let area = hooks.use_previous_size();
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Goal, area, sv);
    panel_shell!(PanelKind::Goal, {
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            state: Some(sv),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: Paragraph::new(ratatui::text::Text::from(lines)).wrap(
                ratatui::widgets::Wrap { trim: false }
            ))
        }
    })
}

fn detail_line(key: &str, text: String, label_style: Style, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{}: ", i18n::tr(key)), label_style),
        Span::styled(text, value_style),
    ])
}

fn status_label(status: Option<GoalStatus>) -> String {
    match status {
        Some(GoalStatus::Active) => i18n::tr("goal-status-active"),
        Some(GoalStatus::Complete) => i18n::tr("goal-status-complete"),
        Some(GoalStatus::Blocked) => i18n::tr("goal-status-blocked"),
        None => i18n::tr("ui-empty"),
    }
}

fn close_panel() {
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_labels_cover_all_states() {
        assert_eq!(
            status_label(Some(GoalStatus::Active)),
            i18n::tr("goal-status-active")
        );
        assert_eq!(
            status_label(Some(GoalStatus::Complete)),
            i18n::tr("goal-status-complete")
        );
        assert_eq!(
            status_label(Some(GoalStatus::Blocked)),
            i18n::tr("goal-status-blocked")
        );
    }
}
