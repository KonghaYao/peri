//! 弹窗组件集合 —— ratatui-kit #[component] 版本。
macro_rules! popup_text_shell {
    ($title:expr, $title_fg:expr, $lines:expr) => {{
        let popup_block = ratatui_kit::ratatui::widgets::Block::default()
            .borders(
                ratatui_kit::ratatui::widgets::Borders::TOP
                    | ratatui_kit::ratatui::widgets::Borders::BOTTOM,
            )
            .border_style(
                ratatui_kit::ratatui::style::Style::new()
                    .fg(peri_theme::atoms::THEME_ATOM.state().read().component.popup.border),
            )
            .title_top(
                ratatui_kit::ratatui::text::Line::from($title)
                    .fg($title_fg)
                    .bold()
                    .centered(),
            );
        let text_render = ratatui_kit::ratatui::widgets::Paragraph::new(
            ratatui_kit::ratatui::text::Text::from($lines),
        )
        .block(popup_block);

        ratatui_kit::element!(
            View(
                flex_direction: ratatui_kit::ratatui::layout::Direction::Vertical,
                width: ratatui_kit::ratatui::layout::Constraint::Fill(1),
                height: ratatui_kit::ratatui::layout::Constraint::Fill(1),
            ) {
                Text(text: text_render)
            }
        )
    }};
}

pub mod ask_user_popup;
pub mod confirm_popup;
pub mod hitl_popup;
pub mod oauth_popup;
pub mod rewind_popup;
