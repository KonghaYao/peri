use peri_widgets::BorderedPanel;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::{
        model_panel::{
            AliasTab, ModelPanel, ROW_1M_CONTEXT, ROW_EFFORT, ROW_HAIKU, ROW_MAX_TOKENS, ROW_OPUS,
            ROW_SONNET,
        },
        App,
    },
    ui::theme,
};

pub(crate) fn render_model_panel(f: &mut Frame, panel: &ModelPanel, app: &mut App, area: Rect) {
    let inner = BorderedPanel::new(Span::styled(
        " Select model ",
        Style::default()
            .fg(theme::THINKING)
            .add_modifier(Modifier::BOLD),
    ))
    .border_style(Style::default().fg(theme::BORDER))
    .render(f, area);

    app.session_mgr.current_mut().ui.panel_area = Some(inner);

    let cfg_guard = app.services.peri_config.read();
    let active_alias_owned = cfg_guard.config.active_alias.clone();
    let active_alias = active_alias_owned.as_str();
    let active_provider_id_owned = cfg_guard.config.active_provider_id.clone();
    let models_ref = cfg_guard
        .config
        .providers
        .iter()
        .find(|p| p.id == active_provider_id_owned)
        .map(|p| &p.models);

    let mut lines: Vec<Line> = Vec::new();

    // Description
    lines.push(Line::from(Span::styled(
        "  Switch between models. Applies to this session.",
        Style::default().fg(theme::MUTED),
    )));
    lines.push(Line::from(""));

    // Model rows: Opus / Sonnet / Haiku
    let rows: [(usize, &AliasTab, &str, &str); 3] = [
        (ROW_OPUS, &AliasTab::Opus, "Opus", "1"),
        (ROW_SONNET, &AliasTab::Sonnet, "Sonnet", "2"),
        (ROW_HAIKU, &AliasTab::Haiku, "Haiku", "3"),
    ];

    for (row_idx, alias, label, num) in &rows {
        let is_active = alias.to_key() == active_alias;
        let is_cursor = panel.cursor() == *row_idx;
        let model_name = models_ref
            .and_then(|m| m.get_model(alias.to_key()))
            .unwrap_or("");

        let check = if is_active { "\u{2714}" } else { " " };
        let cursor_char = if is_cursor { "\u{276f}" } else { " " };

        let label_style = if is_active {
            Style::default()
                .fg(theme::SAGE)
                .add_modifier(Modifier::BOLD)
        } else if is_cursor {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD)
        };

        let model_style = Style::default().fg(theme::MUTED);

        let check_style = if is_active {
            Style::default().fg(theme::SAGE)
        } else {
            Style::default().fg(theme::MUTED)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_char),
                Style::default().fg(theme::THINKING),
            ),
            Span::styled(format!("{}. ", num), label_style),
            Span::styled(format!("{:8}", label), label_style),
            Span::styled(format!(" {}  ", check), check_style),
            Span::styled(model_name.to_string(), model_style),
        ]));
    }

    lines.push(Line::from(""));

    // MaxTokens row
    {
        let is_cursor = panel.cursor() == ROW_MAX_TOKENS;
        let radio_color = if is_cursor {
            theme::THINKING
        } else {
            theme::ACCENT
        };
        let label_style = if is_cursor {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::BOLD)
        };
        let cursor_char = if is_cursor { "\u{276f}" } else { " " };

        let spans = vec![
            Span::styled(
                format!(" {} \u{25cf} ", cursor_char),
                Style::default().fg(radio_color),
            ),
            Span::styled(format!("Max Token: {}", panel.buf_max_tokens), label_style),
        ];

        lines.push(Line::from(spans));
    }

    // Effort row
    {
        let effort_label = match panel.buf_thinking_effort.as_str() {
            "low" => "Low",
            "high" => "High",
            "xhigh" => "XHigh",
            "max" => "Max",
            _ => "Medium",
        };

        let is_cursor = panel.cursor() == ROW_EFFORT;
        let radio_color = if is_cursor {
            theme::THINKING
        } else {
            theme::ACCENT
        };
        let effort_style = if is_cursor {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::BOLD)
        };
        let cursor_char = if is_cursor { "\u{276f}" } else { " " };

        let spans = vec![
            Span::styled(
                format!(" {} \u{25cf} ", cursor_char),
                Style::default().fg(radio_color),
            ),
            Span::styled(format!("Effort: {}", effort_label), effort_style),
        ];

        lines.push(Line::from(spans));
    }

    // 1M Context row
    {
        let state_label = if panel.buf_context_1m { "ON" } else { "OFF" };

        let is_cursor = panel.cursor() == ROW_1M_CONTEXT;
        let radio_color = if is_cursor {
            theme::THINKING
        } else {
            theme::ACCENT
        };
        let label_style = if is_cursor {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::BOLD)
        };
        let cursor_char = if is_cursor { "\u{276f}" } else { " " };

        let state_color = if panel.buf_context_1m {
            theme::SAGE
        } else {
            theme::MUTED
        };

        let spans = vec![
            Span::styled(
                format!(" {} \u{25cf} ", cursor_char),
                Style::default().fg(radio_color),
            ),
            Span::styled("1M Context: ", label_style),
            Span::styled(
                state_label,
                Style::default()
                    .fg(state_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ];

        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));

    lines.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        model_panel::{AliasTab, ModelPanel},
        App,
    };
    include!("model_test.rs");
}
