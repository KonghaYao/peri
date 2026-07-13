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

use crate::i18n;
use crate::kit::atoms;
use crate::kit::atoms::LANG_VERSION;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Alignment, Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::{Borders, Paragraph},
    },
};

#[component]
pub fn SetupWizard(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;
    let component = THEME_ATOM.state().read().component;
    // 订阅 WIZARD_ACTIVE 以便 Esc 关闭后重渲染（虽然 app_shell 也会切走）
    let wizard_active = hooks.use_atom(&atoms::WIZARD_ACTIVE);
    let _ = *wizard_active.read();

    // 订阅 SERVICE_SNAPSHOT 显示当前 Provider 状态
    let snapshot = hooks.use_atom(&atoms::SERVICE_SNAPSHOT);
    let snapshot = snapshot.read().clone();
    let _lang_ver = hooks.use_atom(&LANG_VERSION);
    let provider_name = snapshot.provider_name;
    let model_label = if !snapshot.model_name.is_empty() {
        &snapshot.model_name
    } else {
        &snapshot.model_alias
    };
    let has_provider = !provider_name.is_empty();

    hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }

        match key.code {
            // Esc / Enter / Space：关闭 wizard，进入主界面
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ') => {
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
            Span::styled("✅ ", Style::new().fg(semantic.status.success).bold()),
            Span::styled(
                format!("Provider: {} ({})", provider_name, model_label),
                Style::new().fg(semantic.status.success),
            ),
        ])]
    } else {
        vec![
            Line::from(vec![
                Span::styled("⚠ ", Style::new().fg(semantic.status.error).bold()),
                Span::styled(
                    i18n::tr("setup-no-provider"),
                    Style::new().fg(semantic.status.error).bold(),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                i18n::tr("setup-config-hint-title"),
                Style::new().fg(semantic.text.primary),
            )),
        ]
    };

    let hint_lines = if has_provider {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                i18n::tr("setup-close-hint"),
                Style::new().fg(semantic.text.dim),
            )),
        ]
    } else {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                i18n::tr("setup-step-1"),
                Style::new().fg(semantic.text.primary),
            )),
            Line::from(Span::styled(
                i18n::tr("setup-step-2"),
                Style::new().fg(semantic.text.primary),
            )),
            Line::from(vec![
                Span::styled(i18n::tr("setup-step-3"), Style::new().fg(semantic.text.primary)),
                Span::styled(
                    home_dir.clone(),
                    Style::new().fg(semantic.border.active).italic(),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                i18n::tr("setup-skip-hint"),
                Style::new().fg(semantic.text.dim),
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
                    border_style: Style::new().fg(semantic.border.default),
                    top_title: Line::from(i18n::tr("setup-wizard-title"))
                        .fg(component.message.reasoning)
                        .bold()
                        .centered(),
                    borders: Borders::TOP | Borders::BOTTOM,
                    width: Constraint::Length(72),
                    height: Constraint::Length(16),
                ) {
                    Text(text: Paragraph::new(vec![
                        Line::from(""),
                        Line::from(Span::styled(
                            i18n::tr("setup-welcome"),
                            Style::new().fg(semantic.text.primary).bold(),
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
