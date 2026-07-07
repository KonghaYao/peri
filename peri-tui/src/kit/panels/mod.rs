//! 面板组件集合 —— ratatui-kit #[component] 版本。
macro_rules! panel_shell {
    ($kind:expr, { $($children:tt)* }) => {{
        ratatui_kit::element!(
            Border(
                flex_direction: ratatui_kit::ratatui::layout::Direction::Vertical,
                border_style: ratatui_kit::ratatui::style::Style::new()
                    .fg(crate::kit::theme::component().panel.border),
                borders: ratatui_kit::ratatui::widgets::Borders::TOP
                    | ratatui_kit::ratatui::widgets::Borders::BOTTOM,
                top_title: ratatui_kit::ratatui::text::Line::from(
                    crate::kit::panel_registry::panel_title($kind),
                )
                .fg(crate::kit::theme::component().panel.title)
                .bold()
                .centered(),
                width: ratatui_kit::ratatui::layout::Constraint::Fill(1),
            ) {
                $($children)*
            }
        )
    }};
}

pub mod agent;
pub mod ask_user;
pub mod betas;
pub mod config;
pub mod cron;
pub mod hooks;
pub mod login;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod plugin;
pub mod status;
pub mod tasks;
pub mod thread_browser;
pub mod workflow;
