//! ratatui-kit SetupWizard component.
//!
//! I17-B：从 TODO stub 升级为可用的引导界面。
//!
//! 触发条件：首次启动 `needs_setup() == true`（无 Provider 配置）时，
//! entry.rs 设置 `WIZARD_ACTIVE=true`，AppShell 渲染本组件。
//!
//! 交互：
//! - Esc / q / Enter：关闭 wizard（写入 WIZARD_ACTIVE=false），进入主界面
//!   即使未配置 Provider 也允许跳过——用户可后续通过 Ctrl+, 打开 Settings
//!
//! 引导内容：
//! - 显示当前 Provider 状态（未配置 / 已配置）
//! - 提示用户在主界面中通过 Ctrl+, (Config) / Ctrl+l (Login) 配置
//! - 显示 ~/.peri/settings.json 路径供用户手动编辑

#![allow(clippy::needless_update)]

use crate::kit::atoms;
use crate::kit::theme;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Alignment, Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
pub fn SetupWizard(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // 订阅 WIZARD_ACTIVE 以便 Esc 关闭后重渲染（虽然 app_shell 也会切走）
    let wizard_active = hooks.use_atom(&atoms::WIZARD_ACTIVE);
    let _ = *wizard_active.read();

    // 订阅 SERVICE_SNAPSHOT 显示当前 Provider 状态
    let snapshot = hooks.use_atom(&atoms::SERVICE_SNAPSHOT);
    let snapshot = snapshot.read().clone();
    let provider_name = snapshot.provider_name;
    let model_alias = snapshot.model_alias;
    let has_provider = !provider_name.is_empty();

    hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        match key.code {
            // Esc / q / Enter / Space：关闭 wizard，进入主界面
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char(' ') => {
                *atoms::WIZARD_ACTIVE.state().write() = false;
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    let home_dir = dirs_next::home_dir()
        .map(|h| h.join(".peri").join("settings.json").display().to_string())
        .unwrap_or_else(|| "~/.peri/settings.json".to_string());

    let status_line = if has_provider {
        vec![Line::from(vec![
            Span::styled("● ", Style::new().fg(theme::SAGE).bold()),
            Span::styled(
                format!("Provider: {} ({})", provider_name, model_alias),
                Style::new().fg(theme::SAGE),
            ),
        ])]
    } else {
        vec![
            Line::from(vec![
                Span::styled("● ", Style::new().fg(theme::ERROR).bold()),
                Span::styled(
                    "未配置 Provider — Agent 功能不可用",
                    Style::new().fg(theme::ERROR).bold(),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "要配置 Provider，请选择以下任一方式：",
                Style::new().fg(theme::TEXT),
            )),
        ]
    };

    let hint_lines = if has_provider {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "按 Enter / q / Esc 关闭向导，进入主界面",
                Style::new().fg(theme::DIM),
            )),
        ]
    } else {
        vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  1. ", Style::new().fg(theme::ACCENT).bold()),
                Span::styled(
                    "进入主界面后打开 Login 页面配置 API Key",
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(vec![
                Span::styled("  2. ", Style::new().fg(theme::ACCENT).bold()),
                Span::styled(
                    "或打开 Settings 页面调整 Provider 配置",
                    Style::new().fg(theme::TEXT),
                ),
            ]),
            Line::from(vec![
                Span::styled("  3. ", Style::new().fg(theme::ACCENT).bold()),
                Span::styled("或手动编辑 ", Style::new().fg(theme::TEXT)),
                Span::styled(home_dir.clone(), Style::new().fg(theme::ACCENT).italic()),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "按 Enter / q / Esc 跳过向导，进入主界面",
                Style::new().fg(theme::DIM),
            )),
        ]
    };

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            View(height: Constraint::Fill(1), width: Constraint::Fill(1)) {}
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Length(16),
            ) {
                View(width: Constraint::Fill(1), height: Constraint::Length(16)) {}
                Border(
                    flex_direction: Direction::Vertical,
                    border_style: Style::new().fg(theme::BORDER),
                    top_title: Line::from(" Setup Wizard ").fg(theme::THINKING).bold().centered(),
                    width: Constraint::Length(72),
                    height: Constraint::Length(16),
                ) {
                    Text(text: Paragraph::new(vec![
                        Line::from(""),
                        Line::from(Span::styled(
                            "欢迎使用 Peri TUI",
                            Style::new().fg(theme::TEXT).bold(),
                        )).alignment(Alignment::Center),
                        Line::from(""),
                    ].into_iter().chain(status_line.into_iter()).chain(hint_lines.into_iter()).collect::<Vec<Line<'static>>>())
                        .alignment(Alignment::Left))
                }
                View(width: Constraint::Fill(1), height: Constraint::Length(16)) {}
            }
            View(height: Constraint::Fill(1), width: Constraint::Fill(1)) {}
        }
    )
}
