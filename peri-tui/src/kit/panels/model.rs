//! ratatui-kit ModelPanel component.
//!
//! S6c：alias 列表沿用静态元数据（Opus/Sonnet/Haiku），但**当前激活 alias**
//! 从 `SERVICE_SNAPSHOT` atom 读取——这样面板和 status bar 始终一致。
//! Enter/←→ 操作目前只更新本地 selected_tab state，**真实切换 provider/model**
//! 需要 S11 解耦后通过 AcpClient 触发（暂留 TODO）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, LANG_VERSION, MODEL_HIGHLIGHT_UNTIL, NOTIFICATION, Notification,
    PERI_CONFIG_HANDLE, SERVICE_SNAPSHOT,
};
use crate::kit::list_nav::{next_selection, previous_selection};
use fluent_bundle::FluentValue;
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
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// 静态 alias 元数据（与 active 状态无关）
// ---------------------------------------------------------------------------

struct ModelAliasEntry {
    name: &'static str,
    key: &'static str,
    model_id: &'static str,
}

const MODEL_ALIASES: &[ModelAliasEntry] = &[
    ModelAliasEntry {
        name: "Opus",
        key: "opus",
        model_id: "claude-opus-4-20250514",
    },
    ModelAliasEntry {
        name: "Sonnet",
        key: "sonnet",
        model_id: "claude-sonnet-4-20250514",
    },
    ModelAliasEntry {
        name: "Haiku",
        key: "haiku",
        model_id: "claude-3-5-haiku-20241022",
    },
];

/// 光标导航：alias 行 + 3 个 detail 行
const ALIAS_COUNT: usize = 3;
const IDX_EFFORT: usize = ALIAS_COUNT;
const IDX_MAX_TOKENS: usize = ALIAS_COUNT + 1;
const IDX_CONTEXT_1M: usize = ALIAS_COUNT + 2;
const TOTAL_ITEMS: usize = ALIAS_COUNT + 3;

const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const MAX_TOKEN_PRESETS: &[u32] = &[4096, 8192, 16000, 32000, 64000];

#[component]
pub fn ModelPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);
    // selected_tab stores the index of the selected alias
    let selected_tab = hooks.use_state(|| 1usize); // default Sonnet
    // 渲染版本计数器——cycle_effort/cycle_max_tokens/toggle_context_1m 修改
    // PERI_CONFIG_HANDLE 后递增此计数，触发 ModelPanel 重渲染以显示最新值。
    let render_version = hooks.use_state(|| 0u64);

    // S6c: 订阅 SERVICE_SNAPSHOT——active alias 来自 atom，确保面板和 status bar 一致
    let snapshot = hooks.use_atom(&SERVICE_SNAPSHOT);
    let active_alias = snapshot.read().model_alias.clone();
    let active_provider = snapshot.read().provider_name.clone();
    let active_model_name = snapshot.read().model_name.clone();
    let _ = snapshot; // StoreState 是 Copy，无需显式 drop
    let _lang_ver = hooks.use_atom(&LANG_VERSION);

    // 从 PERI_CONFIG_HANDLE 解析每个 alias 对应的真实模型名
    let alias_names: std::collections::HashMap<&str, String> = {
        PERI_CONFIG_HANDLE
            .get()
            .map(|handle| {
                let cfg = handle.read();
                let active_prov = cfg
                    .config
                    .providers
                    .iter()
                    .find(|p| p.id == cfg.config.active_provider_id);
                MODEL_ALIASES
                    .iter()
                    .map(|entry| {
                        let name = active_prov
                            .and_then(|p| p.models.get_model(entry.key))
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| entry.model_id.to_string());
                        (entry.key, name)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    // 从 PERI_CONFIG_HANDLE 读取 thinking 和 context_1m 实际值
    let (current_effort, current_max_tokens, current_context_1m) = PERI_CONFIG_HANDLE
        .get()
        .map(|handle| {
            let cfg = handle.read();
            let thinking =
                cfg.config
                    .thinking
                    .clone()
                    .unwrap_or_else(|| crate::config::ThinkingConfig {
                        enabled: true,
                        budget_tokens: 8000,
                        effort: "medium".to_string(),
                        max_tokens: 32000,
                    });
            let effort = thinking.effort;
            let max_tokens = thinking.max_tokens;
            let ctx = cfg.config.context_1m.unwrap_or(false);
            (effort, max_tokens, ctx)
        })
        .unwrap_or_else(|| ("medium".to_string(), 32000, false));

    let rv = render_version;
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            match key.code {
                KeyCode::Esc => {}
                KeyCode::Up => {
                    let mut c = cursor.write();
                    *c = previous_selection(*c);
                }
                KeyCode::Down => {
                    let mut c = cursor.write();
                    *c = next_selection(*c, TOTAL_ITEMS);
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Left => {
                    let sel = *cursor.read();
                    match sel {
                        0..=2 => {
                            if key.code == KeyCode::Enter {
                                let new_alias = MODEL_ALIASES[sel].key.to_string();
                                *selected_tab.write() = sel;
                                switch_alias(&new_alias);
                                crate::kit::panel_registry::close_active_panel();
                            } else {
                                let direction = key.code == KeyCode::Right;
                                let mut s = selected_tab.write();
                                *s = if direction {
                                    next_selection(*s, ALIAS_COUNT)
                                } else {
                                    previous_selection(*s)
                                };
                                *cursor.write() = *s;
                            }
                        }
                        IDX_EFFORT => {
                            cycle_effort();
                            *rv.write() += 1;
                        }
                        IDX_MAX_TOKENS => {
                            let forward = key.code == KeyCode::Right || key.code == KeyCode::Enter;
                            cycle_max_tokens(forward);
                            *rv.write() += 1;
                        }
                        IDX_CONTEXT_1M => {
                            toggle_context_1m();
                            *rv.write() += 1;
                        }
                        _ => {}
                    }
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
        Style::new()
            .fg(theme_def.read().semantic.text.primary)
            .bold(),
    )]));
    lines.push(Line::from(vec![
        Span::styled(
            "  Provider: ",
            Style::new().fg(theme_def.read().semantic.text.muted),
        ),
        Span::styled(
            provider_label,
            Style::new()
                .fg(theme_def.read().semantic.border.active)
                .bold(),
        ),
    ]));
    lines.push(Line::from(""));

    // Alias rows
    for (i, entry) in MODEL_ALIASES.iter().enumerate() {
        let is_selected = i == active_idx;
        let is_cursor = i == sel && sel < ALIAS_COUNT;
        let cursor_mark = if is_cursor { "\u{276f}" } else { " " };
        let check = if is_selected { "\u{2714}" } else { " " };

        let name_style = if is_selected {
            Style::new()
                .fg(theme_def.read().semantic.status.success)
                .bold()
        } else if is_cursor {
            Style::new()
                .fg(theme_def.read().component.panel.title)
                .bold()
        } else {
            Style::new().fg(theme_def.read().semantic.text.primary)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme_def.read().component.panel.title),
            ),
            Span::styled(format!("{:<10}", entry.name), name_style),
            Span::styled(
                format!(" {}", check),
                if is_selected {
                    Style::new().fg(theme_def.read().semantic.status.success)
                } else {
                    Style::new().fg(theme_def.read().semantic.text.muted)
                },
            ),
            Span::styled(
                format!(
                    "  {}",
                    alias_names
                        .get(entry.key)
                        .map(|s| s.as_str())
                        .unwrap_or(entry.model_id)
                ),
                Style::new().fg(theme_def.read().semantic.text.muted),
            ),
        ]));
    }

    lines.push(Line::from(""));

    // Current selection info
    lines.push(Line::from(vec![Span::styled(
        format!(
            "  Active: {} → {}",
            active_entry.name, active_entry.model_id
        ),
        Style::new()
            .fg(theme_def.read().semantic.border.active)
            .bold(),
    )]));
    lines.push(Line::from(vec![
        Span::styled(
            "  Model:  ",
            Style::new().fg(theme_def.read().semantic.text.muted),
        ),
        Span::styled(
            active_model_name,
            Style::new().fg(theme_def.read().semantic.text.primary),
        ),
    ]));
    lines.push(Line::from(""));

    // ── 可编辑 Detail 行 ──
    let sel = *cursor.read();

    // Effort
    let is_effort_cursor = sel == IDX_EFFORT;
    lines.push(Line::from(vec![
        Span::styled(
            if is_effort_cursor { "❯ " } else { "  " },
            Style::new().fg(theme_def.read().component.panel.title),
        ),
        Span::styled(
            i18n::tr("model-field-effort"),
            if is_effort_cursor {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.muted)
            },
        ),
        Span::styled(
            format!("  {}", current_effort),
            Style::new()
                .fg(theme_def.read().semantic.status.warning)
                .bold(),
        ),
        if is_effort_cursor {
            Span::styled(
                "  ← → cycle",
                Style::new().fg(theme_def.read().semantic.text.dim).italic(),
            )
        } else {
            Span::styled("", Style::new())
        },
    ]));

    // Max tokens
    let is_max_cursor = sel == IDX_MAX_TOKENS;
    lines.push(Line::from(vec![
        Span::styled(
            if is_max_cursor { "❯ " } else { "  " },
            Style::new().fg(theme_def.read().component.panel.title),
        ),
        Span::styled(
            i18n::tr("model-field-max-token"),
            if is_max_cursor {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.muted)
            },
        ),
        Span::styled(
            current_max_tokens.to_string(),
            Style::new().fg(theme_def.read().semantic.text.primary),
        ),
        if is_max_cursor {
            Span::styled(
                "  ← → cycle",
                Style::new().fg(theme_def.read().semantic.text.dim).italic(),
            )
        } else {
            Span::styled("", Style::new())
        },
    ]));

    // Context 1M
    let is_ctx_cursor = sel == IDX_CONTEXT_1M;
    let ctx_on = current_context_1m;
    lines.push(Line::from(vec![
        Span::styled(
            if is_ctx_cursor { "❯ " } else { "  " },
            Style::new().fg(theme_def.read().component.panel.title),
        ),
        Span::styled(
            i18n::tr("model-field-1m-context"),
            if is_ctx_cursor {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.muted)
            },
        ),
        Span::styled(
            if ctx_on {
                i18n::tr("config-value-on")
            } else {
                i18n::tr("config-value-off")
            },
            if ctx_on {
                Style::new().fg(theme_def.read().semantic.status.success)
            } else {
                Style::new().fg(theme_def.read().semantic.text.muted)
            },
        ),
        if is_ctx_cursor {
            Span::styled(
                i18n::tr("panel-model-inline-toggle-hint"),
                Style::new().fg(theme_def.read().semantic.text.dim).italic(),
            )
        } else {
            Span::styled("", Style::new())
        },
    ]));

    // Footer
    lines.push(Line::from(""));
    lines.push(Line::from(i18n::tr("panel-model-nav-hint")).fg(theme_def.read().semantic.text.dim));

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

fn switch_alias(new_alias: &str) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut cfg = handle.write();
    if cfg.config.active_alias != new_alias {
        cfg.config.active_alias = new_alias.to_string();
        tracing::info!(alias = new_alias, "ModelPanel: active_alias switched");
    }
    let snap = cfg.clone();
    drop(cfg);
    notify_save_result(crate::config::save(&snap));
    let resolved_name = resolve_model_name(&snap.config, new_alias);
    let s_handle = SERVICE_SNAPSHOT.state();
    let mut svc_snap = s_handle.read().clone();
    svc_snap.model_alias = new_alias.to_string();
    svc_snap.model_name = resolved_name;
    *s_handle.write() = svc_snap;
    *MODEL_HIGHLIGHT_UNTIL.state().write() = Some(Instant::now() + Duration::from_secs(2));
    // 推送配置到 ACP 服务端，使 alias 切换立即生效
    tokio::spawn(async move {
        if let Some(client) = ACP_CLIENT_HANDLE.get()
            && let Err(e) = client.update_config(&snap).await
        {
            tracing::warn!(error = %e, "ModelPanel: update_config push failed");
        }
    });
}

fn cycle_effort() {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut cfg = handle.write();
    let thinking = cfg
        .config
        .thinking
        .get_or_insert_with(|| crate::config::ThinkingConfig {
            enabled: true,
            budget_tokens: 8000,
            effort: "medium".to_string(),
            max_tokens: 32000,
        });
    thinking.enabled = true;
    let cur_idx = EFFORT_LEVELS
        .iter()
        .position(|e| e == &thinking.effort)
        .unwrap_or(0);
    thinking.effort = EFFORT_LEVELS[(cur_idx + 1) % EFFORT_LEVELS.len()].to_string();
    let snap = cfg.clone();
    drop(cfg);
    notify_save_result(crate::config::save(&snap));
}

fn cycle_max_tokens(forward: bool) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut cfg = handle.write();
    let thinking = cfg
        .config
        .thinking
        .get_or_insert_with(|| crate::config::ThinkingConfig {
            enabled: true,
            budget_tokens: 8000,
            effort: "medium".to_string(),
            max_tokens: 32000,
        });
    let cur = MAX_TOKEN_PRESETS
        .iter()
        .position(|&v| v == thinking.max_tokens)
        .unwrap_or(0);
    let next = if forward {
        (cur + 1) % MAX_TOKEN_PRESETS.len()
    } else {
        (cur + MAX_TOKEN_PRESETS.len() - 1) % MAX_TOKEN_PRESETS.len()
    };
    thinking.max_tokens = MAX_TOKEN_PRESETS[next];
    let snap = cfg.clone();
    drop(cfg);
    notify_save_result(crate::config::save(&snap));
}

fn toggle_context_1m() {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut cfg = handle.write();
    let cur = cfg.config.context_1m.unwrap_or(false);
    cfg.config.context_1m = Some(!cur);
    let snap = cfg.clone();
    drop(cfg);
    notify_save_result(crate::config::save(&snap));
}

fn resolve_model_name(app_config: &crate::config::AppConfig, alias: &str) -> String {
    let active_provider = app_config
        .providers
        .iter()
        .find(|p| p.id == app_config.active_provider_id);
    active_provider
        .and_then(|p| p.models.get_model(alias))
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| alias.to_string())
}

fn notify_save_result(result: Result<(), anyhow::Error>) {
    match result {
        Ok(()) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("config-saved").to_string(),
                until: Instant::now() + Duration::from_secs(1),
            });
        }
        Err(e) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr_args(
                    "config-save-failed",
                    &[(
                        "error".to_string(),
                        FluentValue::from(e.to_string().as_str()),
                    )],
                ),
                until: Instant::now() + Duration::from_secs(2),
            });
        }
    }
}
