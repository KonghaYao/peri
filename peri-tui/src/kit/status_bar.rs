//! ratatui-kit StatusBar component.
//!
//! S9：完整双行布局——
//! - **Row 1**：权限模式 → cwd basename → provider/model → CPU% → MEM
//!   全部从 SERVICE_SNAPSHOT atom 派生（S5 落地）；高亮计时器控制闪烁。
//! - **Row 2**：状态相关的快捷键 hints（popup/mention/slash/默认 4 态切换）。

use crate::i18n;
use crate::kit::atoms;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Alignment, Constraint, Direction, Flex},
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::time::{Duration, Instant};

/// 状态栏第 1 行：权限模式 · cwd · provider/model · CPU% · MEM · bg tasks
#[component]
fn StatusBarRow1(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&atoms::LANG_VERSION);
    let snap = hooks.use_atom(&atoms::SERVICE_SNAPSHOT);
    let model_hl = hooks.use_atom(&atoms::MODEL_HIGHLIGHT_UNTIL);
    let provider_hl = hooks.use_atom(&atoms::PROVIDER_HIGHLIGHT_UNTIL);
    let mode_hl = hooks.use_atom(&atoms::MODE_HIGHLIGHT_UNTIL);
    let bg_tasks = hooks.use_atom(&atoms::BG_TASKS);
    let ctx_usage = hooks.use_atom(&atoms::CONTEXT_USAGE);

    let snap = snap.read().clone();
    let now = Instant::now();
    let model_highlighted = model_hl.read().as_ref().is_some_and(|t| *t > now);
    let provider_highlighted = provider_hl.read().as_ref().is_some_and(|t| *t > now);
    let mode_highlighted = mode_hl.read().as_ref().is_some_and(|t| *t > now);

    let mut spans: Vec<Span<'static>> = Vec::new();

    // 1. 权限模式
    let mode_label = permission_mode_display(&snap.permission_mode);
    if !mode_label.is_empty() {
        let color = permission_mode_color(&snap.permission_mode);
        let mut style = Style::default().fg(color);
        if mode_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(format!(" {}", mode_label), style));
    }

    // 2. cwd basename
    spans.push(separator());
    spans.push(Span::styled(
        cwd_basename(&snap.cwd),
        Style::default().fg(statusbar().muted),
    ));

    // 3. provider/model —— 整体统一样式
    let model_display = if !snap.model_name.is_empty() {
        &snap.model_name
    } else if !snap.model_alias.is_empty() {
        &snap.model_alias
    } else {
        ""
    };

    if !snap.provider_name.is_empty() && !model_display.is_empty() {
        spans.push(separator());
        let mut style = Style::default().fg(THEME_ATOM.state().read().semantic.accent);
        if provider_highlighted && model_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        } else if provider_highlighted {
            style = style.add_modifier(Modifier::BOLD);
        } else if model_highlighted {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(
            format!(" {}/{}", snap.provider_name, model_display),
            style,
        ));
    }

    // 4. CPU%（仅在超过 50% 时显示）
    if snap.cpu_percent > 50.0 {
        spans.push(separator());
        spans.push(Span::styled(
            format!("CPU {:.0}%", snap.cpu_percent),
            Style::default().fg(resource_color_by_load(snap.cpu_percent as f64, 50.0, 100.0)),
        ));
    }

    // 5. MEM
    spans.push(separator());
    spans.push(Span::styled(
        format!("MEM {}MB", snap.memory_mb),
        Style::default().fg(memory_color(snap.memory_mb)),
    ));

    // 6. 后台任务计数
    let bg = bg_tasks.read();
    let shell_c = bg.iter().filter(|t| t.kind == "shell").count();
    let agent_c = bg.iter().filter(|t| t.kind == "agent").count();
    let wf_c = bg.iter().filter(|t| t.kind == "workflow").count();
    if shell_c > 0 || agent_c > 0 || wf_c > 0 {
        spans.push(separator());
        let mut parts = vec![];
        if shell_c > 0 {
            parts.push(format!("{} shell", shell_c));
        }
        if agent_c > 0 {
            parts.push(format!("{} agent", agent_c));
        }
        if wf_c > 0 {
            parts.push(format!("{} workflow", wf_c));
        }
        spans.push(Span::styled(
            parts.join(" "),
            Style::default().fg(THEME_ATOM.state().read().semantic.loading),
        ));
    }

    // 7. 上下文使用率（放最后，与旧架构 status_bar render_first_row 一致）
    if let Some((pct, total)) = ctx_usage.read().as_ref() {
        // pct 已经是百分比值（0-100），来自 StateSnapshotMeta.budget_pct（agent 侧 context_usage_percent）
        let pct_display = *pct;
        let total_display = if *total >= 1_000_000 {
            format!("{:.0}M", *total as f64 / 1_000_000.0)
        } else {
            format!("{:.0}k", *total as f64 / 1000.0)
        };
        let color = if pct_display >= 85.0 {
            statusbar().resource_bad
        } else if pct_display >= 70.0 {
            statusbar().resource_warn
        } else {
            statusbar().resource_good
        };
        spans.push(separator());
        spans.push(Span::styled(
            format!("{:.0}% {}", pct_display, total_display),
            Style::default().fg(color),
        ));
    }

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
        ) {
            Text(text: Paragraph::new(Line::from(spans)))
        }
    )
}

/// 状态栏第 2 行：状态相关的快捷键 hints + 复制提示
#[component]
fn StatusBarRow2(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&atoms::LANG_VERSION);
    // I19-C：原代码读 POPUP_ACTIVE（dead atom，open/close_popup 从不同步）
    // 导致 popup hints 永远不显示。改读 POPUP_KIND.is_some()。
    let popup_kind = hooks.use_atom(&atoms::POPUP_KIND);
    let at_active = hooks.use_atom(&atoms::AT_MENTION_ACTIVE);
    let slash_active = hooks.use_atom(&atoms::SLASH_HINT_ACTIVE);
    let copy_until = hooks.use_atom(&atoms::COPY_MESSAGE_UNTIL);
    let copy_count = hooks.use_atom(&atoms::COPY_CHAR_COUNT);
    let quit_pending = hooks.use_atom(&atoms::QUIT_PENDING_SINCE);

    let is_popup = popup_kind.read().is_some();
    let is_at = *at_active.read();
    let is_slash = *slash_active.read();
    let now = Instant::now();

    // 复制提示优先于其他 hints。
    // [TRAP] 只读 atom 判断过期——禁止在 render body 中写 atom（render→write→render 自激）。
    // mark_copy_message 总是用新 Instant 覆盖 atom，旧 Some(until) 残留不影响下次显示。
    let copy_active = copy_until.read().is_some_and(|until| now < until);
    if copy_active {
        let char_count = *copy_count.read();
        let hint = i18n::tr_args(
            "statusbar-copied",
            &[("count".to_string(), FluentValue::from(char_count as u64))],
        );
        return element!(
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Length(1),
                justify_content: Flex::Center,
            ) {
                Text(text: Paragraph::new(
                    Line::from(hint).fg(statusbar().text)
                ).centered())
            }
        );
    }

    // Ctrl+C 退出待确认提示——在 hint 行显示，不挤占通知栏。
    let quit_active = quit_pending
        .read()
        .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1));
    if quit_active {
        let hint = i18n::tr("statusbar-hint-quit-pending");
        return element!(
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Length(1),
                justify_content: Flex::End,
            ) {
                Text(text: Paragraph::new(
                    Line::from(hint).fg(statusbar().text)
                ).right_aligned())
            }
        );
    }

    let hints = if is_popup {
        Line::from(i18n::tr("statusbar-hint-popup")).fg(statusbar().muted)
    } else if is_at || is_slash {
        Line::from(i18n::tr("statusbar-hint-menu")).fg(statusbar().muted)
    } else {
        Line::from(i18n::tr("statusbar-hint-main")).fg(statusbar().muted)
    };

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Length(1),
            justify_content: Flex::End,
        ) {
            Text(text: Paragraph::new(hints), alignment: Alignment::Right)
        }
    )
}

#[component]
pub fn StatusBar(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _lang = hooks.use_atom(&atoms::LANG_VERSION);
    // 通知条：渲染前检查过期——不写 atom，过期自动忽略。
    // 下次事件处理器写 NOTIFICATION 会用新值覆盖旧 Some。
    let notif_store = hooks.use_atom(&atoms::NOTIFICATION);
    let show_notif = notif_store
        .read()
        .as_ref()
        .is_some_and(|n| Instant::now() < n.until);
    let notif_text = if show_notif {
        notif_store
            .read()
            .as_ref()
            .map(|n| n.message.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let statusbar_tokens = statusbar();
    let notif_line = if show_notif && !notif_text.is_empty() {
        element!(
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Length(1),
            ) {
                Text(text: Paragraph::new(
                    Line::from(Span::styled(notif_text, Style::default().fg(statusbar_tokens.text).add_modifier(Modifier::BOLD)))
                ))
            }
        )
    } else {
        element!(
            View(
                flex_direction: Direction::Horizontal,
                width: Constraint::Fill(1),
                height: Constraint::Length(0),
            ) {
                Text(text: Paragraph::new(Line::from("")))
            }
        )
    };

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(4),
        ) {
            { notif_line }
            StatusBarRow1()
            StatusBarRow2()
            // 第 4 行留空（视觉缓冲）
            Text(text: Paragraph::new(Line::from("")))
        }
    )
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────

fn statusbar() -> peri_theme::component::StatusBarTokens {
    THEME_ATOM.state().read().component.statusbar
}

fn separator() -> Span<'static> {
    Span::styled(" · ", Style::default().fg(statusbar().muted))
}

/// 把 atom 中的 permission_mode 字符串映射为显示标签。
fn permission_mode_display(mode: &str) -> String {
    match mode {
        "accept-edit" => i18n::tr("statusbar-permission-accept-edit"),
        "auto-mode" => i18n::tr("statusbar-permission-auto"),
        "bypass" => i18n::tr("statusbar-permission-bypass"),
        _ => i18n::tr("statusbar-permission-dont-ask"),
    }
}

fn permission_mode_color(mode: &str) -> ratatui::style::Color {
    match mode {
        "accept-edit" => statusbar().mode_accept_edit,
        "auto-mode" => statusbar().mode_auto,
        "bypass" => statusbar().mode_bypass,
        _ => statusbar().text,
    }
}

/// 从 cwd 路径取 basename（最后一节）。空或异常返回原串。
fn cwd_basename(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| cwd.to_string())
}

/// 通用阈值染色：>= high → ERROR，>= low → WARNING，else SAGE。
fn resource_color_by_load(value: f64, low: f64, high: f64) -> ratatui::style::Color {
    if value >= high {
        statusbar().resource_bad
    } else if value >= low {
        statusbar().resource_warn
    } else {
        statusbar().resource_good
    }
}

/// MEM 染色：>1024 ERROR，>512 WARNING，else SAGE（与 legacy status_bar 一致）。
fn memory_color(mem_mb: u64) -> ratatui::style::Color {
    if mem_mb > 1024 {
        statusbar().resource_bad
    } else if mem_mb > 512 {
        statusbar().resource_warn
    } else {
        statusbar().resource_good
    }
}

#[cfg(test)]
#[path = "status_bar_test.rs"]
mod tests;
