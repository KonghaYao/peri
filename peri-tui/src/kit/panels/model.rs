//! ratatui-kit ModelPanel component.
//!
//! S6c：alias 列表沿用静态元数据（Opus/Sonnet/Haiku），但**当前激活 alias**
//! 从 `SERVICE_SNAPSHOT` atom 读取——这样面板和 status bar 始终一致。
//! Enter/←→ 操作目前只更新本地 selected_tab state，**真实切换 provider/model**
//! 需要 S11 解耦后通过 AcpClient 触发（暂留 TODO）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    LANG_VERSION, MODEL_HIGHLIGHT_UNTIL, PERI_CONFIG_HANDLE, SERVICE_SNAPSHOT,
};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::theme;
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
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// 静态 alias 元数据（与 active 状态无关）
// ---------------------------------------------------------------------------

struct ModelAliasEntry {
    name: &'static str,
    key: &'static str,
    effort: &'static str,
    max_tokens: u32,
    context_1m: bool,
    model_id: &'static str,
}

const MODEL_ALIASES: &[ModelAliasEntry] = &[
    ModelAliasEntry {
        name: "Opus",
        key: "opus",
        effort: "high",
        max_tokens: 32000,
        context_1m: false,
        model_id: "claude-opus-4-20250514",
    },
    ModelAliasEntry {
        name: "Sonnet",
        key: "sonnet",
        effort: "high",
        max_tokens: 64000,
        context_1m: false,
        model_id: "claude-sonnet-4-20250514",
    },
    ModelAliasEntry {
        name: "Haiku",
        key: "haiku",
        effort: "low",
        max_tokens: 8000,
        context_1m: false,
        model_id: "claude-3-5-haiku-20241022",
    },
];

#[component]
pub fn ModelPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);
    // selected_tab stores the index of the selected alias
    let selected_tab = hooks.use_state(|| 1usize); // default Sonnet

    // S6c: 订阅 SERVICE_SNAPSHOT——active alias 来自 atom，确保面板和 status bar 一致
    let snapshot = hooks.use_atom(&SERVICE_SNAPSHOT);
    let active_alias = snapshot.read().model_alias.clone();
    let active_provider = snapshot.read().provider_name.clone();
    let _ = snapshot; // StoreState 是 Copy，无需显式 drop
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            match key.code {
                KeyCode::Esc => {
                    // 由 PanelOverlay 上层 Esc 处理关闭
                }
                KeyCode::Up => {
                    let mut c = cursor.write();
                    *c = previous_selection(*c);
                }
                KeyCode::Down => {
                    let mut c = cursor.write();
                    *c = next_selection(*c, MODEL_ALIASES.len());
                }
                KeyCode::Enter => {
                    // H2: 通过 PERI_CONFIG_HANDLE 直接 write active_alias。
                    // ACP server 持同一 Arc，立即生效。
                    let sel = *cursor.read();
                    *selected_tab.write() = sel;
                    if let Some(handle) = PERI_CONFIG_HANDLE.get() {
                        let new_alias = MODEL_ALIASES[sel].key.to_string();
                        let mut cfg = handle.write();
                        if cfg.config.active_alias != new_alias {
                            cfg.config.active_alias = new_alias.clone();
                            tracing::info!(
                                alias = MODEL_ALIASES[sel].key,
                                "ModelPanel: active_alias switched"
                            );
                        }
                        drop(cfg);
                        // S6c: 即时推送 SERVICE_SNAPSHOT，避免等待 2s 后台轮询。
                        // write() 返回 ReactiveMutRef，Drop 时自动唤醒所有订阅者（含 StatusBar）。
                        let handle = SERVICE_SNAPSHOT.state();
                        let mut snap = handle.read().clone();
                        snap.model_alias = new_alias;
                        *handle.write() = snap;
                        // 激活动画闪烁：StatusBar 已有 MODEL_HIGHLIGHT_UNTIL 订阅，
                        // 切换后 2s 内 model_alias 文字 BOLD + SLOW_BLINK。
                        *MODEL_HIGHLIGHT_UNTIL.state().write() =
                            Some(Instant::now() + Duration::from_secs(2));
                    }
                    // 关闭面板：I19-A 弹栈而非清空整个栈
                    crate::kit::panel_registry::close_active_panel();
                }
                KeyCode::Left => {
                    let mut s = selected_tab.write();
                    *s = previous_selection(*s);
                    *cursor.write() = *s;
                }
                KeyCode::Right => {
                    let mut s = selected_tab.write();
                    *s = next_selection(*s, MODEL_ALIASES.len());
                    *cursor.write() = *s;
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    let sel = *cursor.read();
    let local_selected = *selected_tab.read();

    // active alias 优先取 atom；atom 为空时回退到本地 selected_tab
    let active_idx = MODEL_ALIASES
        .iter()
        .position(|e| e.key.eq_ignore_ascii_case(&active_alias))
        .unwrap_or(local_selected);
    let active_entry = &MODEL_ALIASES[active_idx];
    let provider_label = if active_provider.is_empty() {
        i18n::tr("app-not-configured")
    } else {
        active_provider.clone()
    };

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Header
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("model-panel-title"),
        Style::new().fg(theme::semantic().text.primary).bold(),
    )]));
    lines.push(Line::from(vec![
        Span::styled(
            "  Provider: ",
            Style::new().fg(theme::semantic().text.muted),
        ),
        Span::styled(
            provider_label,
            Style::new().fg(theme::semantic().border.active).bold(),
        ),
    ]));
    lines.push(Line::from(""));

    // Alias rows
    for (i, entry) in MODEL_ALIASES.iter().enumerate() {
        let is_selected = i == active_idx;
        let is_cursor = i == sel;
        let cursor_mark = if is_cursor { "\u{276f}" } else { " " };
        let check = if is_selected { "\u{2714}" } else { " " };

        let name_style = if is_selected {
            Style::new().fg(theme::semantic().status.success).bold()
        } else if is_cursor {
            Style::new().fg(theme::component().panel.title).bold()
        } else {
            Style::new().fg(theme::semantic().text.primary)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme::component().panel.title),
            ),
            Span::styled(format!("{:<10}", entry.name), name_style),
            Span::styled(
                format!(" {}", check),
                if is_selected {
                    Style::new().fg(theme::semantic().status.success)
                } else {
                    Style::new().fg(theme::semantic().text.muted)
                },
            ),
            Span::styled(
                format!("  {}", entry.model_id),
                Style::new().fg(theme::semantic().text.muted),
            ),
        ]));
    }

    lines.push(Line::from(""));

    // Current selection details
    lines.push(Line::from(vec![Span::styled(
        format!("  Active: {}", active_entry.name),
        Style::new().fg(theme::semantic().border.active).bold(),
    )]));
    lines.push(Line::from(vec![
        Span::styled(
            "  Model ID: ",
            Style::new().fg(theme::semantic().text.muted),
        ),
        Span::styled(
            active_entry.model_id,
            Style::new().fg(theme::semantic().text.primary),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            i18n::tr("model-field-effort"),
            Style::new().fg(theme::semantic().text.muted),
        ),
        Span::styled(
            active_entry.effort,
            Style::new().fg(theme::semantic().status.warning).bold(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            i18n::tr("model-field-max-token"),
            Style::new().fg(theme::semantic().text.muted),
        ),
        Span::styled(
            active_entry.max_tokens.to_string(),
            Style::new().fg(theme::semantic().text.primary),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(
            i18n::tr("model-field-1m-context"),
            Style::new().fg(theme::semantic().text.muted),
        ),
        Span::styled(
            if active_entry.context_1m {
                i18n::tr("config-value-on")
            } else {
                i18n::tr("config-value-off")
            },
            if active_entry.context_1m {
                Style::new().fg(theme::semantic().status.success)
            } else {
                Style::new().fg(theme::semantic().text.muted)
            },
        ),
    ]));

    // Footer
    lines.push(Line::from(""));
    lines.push(
        Line::from("  ↑/↓::navigate  Enter::open  ←/→::switch").fg(theme::semantic().text.dim),
    );

    let content = Paragraph::new(ratatui::text::Text::from(lines));
    panel_shell!(PanelKind::Model, {
        ScrollView(
            scrollbars: crate::kit::panel_registry::clean_scrollbars(),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: content)
        }
    })
}
