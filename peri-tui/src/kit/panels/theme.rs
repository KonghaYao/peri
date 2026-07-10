//! ratatui-kit ThemePanel component.
//!
//! 列出可用主题（builtin + ~/.peri/themes/），显示当前选中，
//! Enter 切换主题，Esc 关闭。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

use crate::app::panel_types::PanelKind;
use crate::kit::list_nav::{next_selection, previous_selection};
use peri_theme::atoms::{PALETTE_ATOM, PERI_COLORS_ATOM, THEME_ATOM};
use peri_theme::bridge::ThemeDefinitionExt;
use peri_theme::loader::list_available_themes;
use std::sync::OnceLock;

static THEME_LIST: OnceLock<Vec<String>> = OnceLock::new();

fn get_theme_list() -> &'static Vec<String> {
    THEME_LIST.get_or_init(list_available_themes)
}

#[component]
pub fn ThemePanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| {
        let themes = get_theme_list();
        let current = theme_def.read().name.to_string();
        themes.iter().position(|name| *name == current).unwrap_or(0)
    });

    let current_name = theme_def.read().name.to_string();
    let themes = get_theme_list();

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        let count = themes.len();
        move |event| {
            if let Event::Key(key) = event {
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            let mut s = selected.write();
                            *s = previous_selection(*s);
                            return EventResult::Consumed;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let mut s = selected.write();
                            *s = next_selection(*s, count);
                            return EventResult::Consumed;
                        }
                        KeyCode::Enter => {
                            let idx = *selected.read();
                            if let Some(name) = themes.get(idx) {
                                switch_theme(name);
                            }
                            return EventResult::Consumed;
                        }
                        _ => {}
                    }
                }
            }
            EventResult::Ignored
        }
    });

    let selection = *selected.read();
    let guard = theme_def.read();

    let mut lines: Vec<Line<'_>> = Vec::new();
    for (i, name) in themes.iter().enumerate() {
        let is_current = *name == current_name;
        let cursor = if i == selection { ">" } else { " " };
        let active_mark = if is_current { " *" } else { "" };
        let display = format!("{} {}{}", cursor, name, active_mark);

        let style = if i == selection {
            Style::new().fg(guard.component.panel.title).bold()
        } else if is_current {
            Style::new().fg(guard.semantic.status.success)
        } else {
            Style::new().fg(guard.semantic.text.primary)
        };

        lines.push(Line::from(Span::styled(display, style)));
    }

    let footer =
        Line::from("  ↑/↓::navigate  Enter::switch  Esc::close").fg(guard.semantic.text.dim);

    let content = if lines.is_empty() {
        Paragraph::new(Line::from("  (no themes found)").fg(guard.semantic.text.muted))
    } else {
        Paragraph::new(ratatui::text::Text::from(lines))
    };

    panel_shell!(
        PanelKind::Theme,
        {
            View(
                flex_direction: ratatui_kit::ratatui::layout::Direction::Vertical,
                width: ratatui_kit::ratatui::layout::Constraint::Fill(1),
                height: ratatui_kit::ratatui::layout::Constraint::Fill(1),
            ) {
                Text(text: content)
                Text(text: Paragraph::new(ratatui::text::Text::from(footer)))
            }
        }
    )
}

fn switch_theme(name: &str) {
    match peri_theme::loader::load_theme(name) {
        Ok(theme) => {
            let palette = theme.to_palette();
            let peri = std::sync::Arc::new(theme.to_peri_colors());

            let mut theme_state = THEME_ATOM.state();
            theme_state.set(theme);

            let mut palette_state = PALETTE_ATOM.state();
            palette_state.set(palette);

            let mut peri_state = PERI_COLORS_ATOM.state();
            peri_state.set(peri);
        }
        Err(e) => {
            tracing::error!("failed to switch theme to '{}': {}", name, e);
        }
    }
}
