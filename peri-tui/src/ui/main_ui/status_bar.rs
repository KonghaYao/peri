use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::{AgentPanel, App},
    ui::theme,
};

pub(crate) fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_first_row(f, app, rows[0]);
    render_second_row(f, app, rows[1]);
    // 第三行留空，作为视觉缓冲
}

/// 第一行：权限模式 │ 工作目录 │ 模型名
fn render_first_row(f: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();

    // 权限模式标签
    {
        use peri_middlewares::prelude::PermissionMode;
        let mode = app.services.permission_mode.load();
        let (label, color) = match mode {
            PermissionMode::Default => ("", theme::TEXT),
            PermissionMode::AcceptEdit => ("Accept Edit", theme::THINKING),
            PermissionMode::AutoMode => ("Auto Mode", theme::WARNING),
            PermissionMode::Bypass => ("Bypass", theme::ERROR),
        };

        // Default 模式不显示标签
        if !label.is_empty() {
            let is_highlight = app
                .global_ui
                .mode_highlight_until
                .is_some_and(|until| std::time::Instant::now() < until);
            let mut style = Style::default().fg(color);
            if is_highlight {
                style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
            }
            spans.push(Span::styled(format!(" {}", label), style));
        }
    }

    // 工作目录
    spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
    let cwd_short = std::path::Path::new(&app.services.cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&app.services.cwd);
    spans.push(Span::styled(
        cwd_short.to_string(),
        Style::default().fg(theme::MUTED),
    ));

    // 模型名（只显示 model name）
    spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
    {
        let is_highlight = app
            .global_ui
            .model_highlight_until
            .is_some_and(|until| std::time::Instant::now() < until);
        let mut style = Style::default().fg(theme::MODEL_INFO);
        if is_highlight {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
        }
        spans.push(Span::styled(format!(" {}", app.services.model_name), style));
    }

    // 进程资源监控
    {
        let mut monitor = app.services.resource_monitor.lock();
        monitor.refresh_if_needed();
        let mem = monitor.memory_mb();
        drop(monitor); // 释放锁后再渲染

        let mem_color = if mem > 1024 {
            theme::ERROR
        } else if mem > 512 {
            theme::WARNING
        } else {
            theme::SAGE
        };

        spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
        spans.push(Span::styled(
            format!("MEM {}MB", mem),
            Style::default().fg(mem_color),
        ));
    }

    // 上下文使用率（放最后）
    {
        let tracker = &app.session_mgr.current().agent.session_token_tracker;
        if let Some(pct) =
            tracker.context_usage_percent(app.session_mgr.current().agent.context_window)
        {
            let total = app.session_mgr.current().agent.context_window;
            let color = if pct >= 85.0 {
                theme::ERROR
            } else if pct >= 70.0 {
                theme::WARNING
            } else {
                theme::SAGE
            };
            spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
            let total_display = if total >= 1_000_000 {
                format!("{:.0}M", total as f64 / 1_000_000.0)
            } else {
                format!("{:.0}k", total as f64 / 1000.0)
            };
            spans.push(Span::styled(
                format!("{:.0}% {}", pct, total_display),
                Style::default().fg(color),
            ));
        }
    }

    render_truncated_line(f, spans, Vec::new(), area);
}

/// 第二行：[Agent 面板信息] │ [快捷键提示]
fn render_second_row(f: &mut Frame, app: &App, area: Rect) {
    let lc = &app.services.lc;
    let mut left_spans: Vec<Span> = Vec::new();
    let mut has_content = false;

    // 复制成功提示
    if let Some(until) = app.session_mgr.current().ui.copy_message_until {
        if std::time::Instant::now() < until {
            let count = app.session_mgr.current().ui.copy_char_count;
            left_spans.push(Span::styled(
                format!(
                    " {}",
                    lc.tr_args(
                        "statusbar-copied",
                        &[("count".into(), (count as i64).into()),]
                    )
                ),
                Style::default().fg(theme::MUTED),
            ));
            has_content = true;
        }
    }

    // 后台任务指示器
    if !app.session_mgr.current().background_agents.is_empty() {
        if has_content {
            left_spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
        }
        left_spans.push(Span::styled(
            lc.tr_args(
                "statusbar-bg-indicator",
                &[(
                    "count".into(),
                    (app.session_mgr.current().background_agents.len() as i64).into(),
                )],
            ),
            Style::default().fg(theme::WARNING),
        ));
        has_content = true;
    }

    // Agent 面板信息（仅面板激活时）
    if let Some(panel) = app.session_mgr.current().session_panels.get::<AgentPanel>() {
        if has_content {
            left_spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
        }
        if let Some(agent) = panel.current_agent() {
            left_spans.push(Span::styled(
                format!(" {}", agent.name),
                Style::default().fg(theme::MUTED),
            ));
        } else {
            left_spans.push(Span::styled(
                format!(" {}", lc.tr("statusbar-no-agent")),
                Style::default().fg(theme::MUTED),
            ));
        }
    } else if let Some(id) = app.get_agent_id() {
        if has_content {
            left_spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
        }
        left_spans.push(Span::styled(
            format!(" {}", id),
            Style::default().fg(theme::MUTED),
        ));
    }

    // 重试状态（放在左侧）
    if let Some(ref retry) = app.session_mgr.current().agent.retry_status {
        if has_content {
            left_spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
        }
        let delay_sec = retry.delay_ms as f64 / 1000.0;
        let err_preview: String = retry.error.chars().take(60).collect();
        let err_display = if retry.error.chars().count() > 60 {
            format!("{}...", err_preview)
        } else {
            err_preview
        };
        left_spans.push(Span::styled(
            format!(
                " {}",
                lc.tr_args(
                    "statusbar-retrying",
                    &[
                        ("attempt".into(), (retry.attempt as i64).into()),
                        ("max".into(), (retry.max_attempts as i64).into()),
                        ("delay".into(), format!("{:.1}", delay_sec).into()),
                        ("error".into(), err_display.into()),
                    ]
                )
            ),
            Style::default().fg(theme::WARNING),
        ));
    }

    // MCP 初始化进度（瞬时事件）
    if let Some(ref rx) = app.services.mcp_init_rx {
        let status = rx.borrow().clone();
        use peri_middlewares::mcp::McpInitStatus;
        match status {
            McpInitStatus::Initializing { connected, total } => {
                // 重置失败提示计时器：进入 Initializing 说明正在重试
                app.global_ui.mcp_failed_shown_until.set(None);
                if has_content {
                    left_spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
                }
                left_spans.push(Span::styled(
                    lc.tr_args(
                        "statusbar-mcp-connecting",
                        &[
                            ("connected".into(), (connected as i64).into()),
                            ("total".into(), (total as i64).into()),
                        ],
                    ),
                    Style::default().fg(theme::MUTED),
                ));
                has_content = true;
            }
            McpInitStatus::Ready { total } if total > 0 => {
                // 重置失败提示计时器：Ready 说明连接已恢复
                app.global_ui.mcp_failed_shown_until.set(None);
                if app.global_ui.mcp_ready_shown_until.get().is_none() {
                    app.global_ui.mcp_ready_shown_until.set(Some(
                        std::time::Instant::now() + std::time::Duration::from_secs(3),
                    ));
                }
                if let Some(until) = app.global_ui.mcp_ready_shown_until.get() {
                    if std::time::Instant::now() < until {
                        if has_content {
                            left_spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
                        }
                        left_spans.push(Span::styled(
                            lc.tr_args(
                                "statusbar-mcp-ready",
                                &[("total".into(), (total as i64).into())],
                            ),
                            Style::default().fg(theme::SAGE),
                        ));
                        has_content = true;
                    }
                }
            }
            McpInitStatus::Failed(ref msg) => {
                // 自消失计时器：首次显示后 10 秒自动消失
                if app.global_ui.mcp_failed_shown_until.get().is_none() {
                    app.global_ui.mcp_failed_shown_until.set(Some(
                        std::time::Instant::now() + std::time::Duration::from_secs(10),
                    ));
                }
                if let Some(until) = app.global_ui.mcp_failed_shown_until.get() {
                    if std::time::Instant::now() < until {
                        if has_content {
                            left_spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
                        }
                        left_spans.push(Span::styled(
                            lc.tr_args(
                                "statusbar-mcp-failed",
                                &[("msg".into(), msg.clone().into())],
                            ),
                            Style::default().fg(theme::ERROR),
                        ));
                        has_content = true;
                    }
                }
            }
            McpInitStatus::Pending | McpInitStatus::Ready { .. } => {}
        }
    }

    // LSP 诊断计数（瞬时事件）
    {
        let agent = &app.session_mgr.current().agent;
        if agent.lsp_diagnostics.errors > 0 || agent.lsp_diagnostics.warnings > 0 {
            if has_content {
                left_spans.push(Span::styled(" · ", Style::default().fg(theme::MUTED)));
            }
            left_spans.push(Span::styled(
                lc.tr_args(
                    "statusbar-lsp-diag",
                    &[
                        (
                            "errors".into(),
                            (agent.lsp_diagnostics.errors as i64).into(),
                        ),
                        (
                            "warnings".into(),
                            (agent.lsp_diagnostics.warnings as i64).into(),
                        ),
                    ],
                ),
                Style::default().fg(theme::MUTED),
            ));
        }
    }

    // Rewind 忙碌提示
    if let Some(until) = app.global_ui.rewind_busy_hint_until {
        if std::time::Instant::now() < until {
            left_spans.push(Span::styled(
                lc.tr("statusbar-rewind-wait"),
                Style::default().fg(theme::WARNING),
            ));
        }
    }

    // Rewind 待确认提示（第一次 ESC 后显示）
    if let Some(since) = app.global_ui.rewind_pending_since {
        if since.elapsed() < std::time::Duration::from_secs(2) {
            left_spans.push(Span::styled(
                lc.tr("statusbar-rewind-pending"),
                Style::default().fg(theme::ACCENT),
            ));
        }
    }

    // 右侧：快捷键提示（统一灰色显示）
    let key_style = Style::default()
        .fg(theme::MUTED)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(theme::MUTED);

    let right_spans: Vec<Span> = match &app.session_mgr.current().agent.interaction_prompt {
        Some(_) if app.global_ui.oauth_prompt.is_some() => {
            let lc = &app.services.lc;
            format_hints(
                &[
                    ("Ctrl+O".to_string(), lc.tr("key-open-browser")),
                    ("Enter".to_string(), lc.tr("key-submit")),
                    ("Esc".to_string(), lc.tr("key-cancel")),
                ],
                key_style,
                desc_style,
            )
        }
        Some(crate::app::InteractionPrompt::Questions(_)) => {
            let lc = &app.services.lc;
            format_hints(
                &[
                    ("Tab".to_string(), lc.tr("key-switch")),
                    ("↑↓".to_string(), lc.tr("key-move")),
                    ("Space".to_string(), lc.tr("key-select")),
                    ("Enter".to_string(), lc.tr("key-confirm")),
                ],
                key_style,
                desc_style,
            )
        }
        Some(crate::app::InteractionPrompt::Approval(_)) => {
            let lc = &app.services.lc;
            format_hints(
                &[
                    ("↑↓".to_string(), lc.tr("key-move")),
                    ("Space".to_string(), lc.tr("key-switch")),
                    ("Enter".to_string(), lc.tr("key-confirm")),
                ],
                key_style,
                desc_style,
            )
        }
        Some(crate::app::InteractionPrompt::Rewind(prompt)) => {
            use crate::app::RewindMode;
            match prompt.mode {
                RewindMode::ConfirmRevert => format_hints(
                    &[
                        ("Enter".to_string(), lc.tr("key-confirm")),
                        ("Esc".to_string(), lc.tr("key-cancel")),
                    ],
                    key_style,
                    desc_style,
                ),
                _ => format_hints(
                    &[
                        ("↑↓".to_string(), lc.tr("statusbar-rewind-move")),
                        ("Tab".to_string(), lc.tr("statusbar-rewind-switch-file")),
                        ("Enter".to_string(), lc.tr("key-confirm")),
                        ("Esc".to_string(), lc.tr("key-cancel")),
                    ],
                    key_style,
                    desc_style,
                ),
            }
        }
        None => {
            let no_mouse = app.global_ui.mouse_available == Some(false);
            let lc = &app.services.lc;
            let hints = if app.session_mgr.current().session_panels.is_any_open() {
                app.session_mgr
                    .current()
                    .session_panels
                    .status_bar_hints(lc)
            } else if app.global_panels.is_any_open() {
                app.global_panels.status_bar_hints(lc)
            } else if app.global_ui.rewind_pending_since.is_some() {
                vec![
                    ("Esc".to_string(), lc.tr("statusbar-rewind-action")),
                    (
                        lc.tr("statusbar-rewind-other-key").to_string(),
                        lc.tr("key-cancel"),
                    ),
                ]
            } else if app.global_ui.quit_pending_since.is_some() {
                vec![
                    ("Ctrl+C".to_string(), lc.tr("key-close")),
                    (
                        lc.tr("statusbar-rewind-other-key").to_string(),
                        lc.tr("key-cancel"),
                    ),
                ]
            } else if no_mouse {
                vec![
                    ("/".to_string(), lc.tr("key-command")),
                    ("Shift+Enter".to_string(), lc.tr("key-newline")),
                    ("Ctrl+T".to_string(), lc.tr("key-switch-model")),
                    ("Ctrl+U/D".to_string(), lc.tr("key-scroll")),
                ]
            } else {
                vec![
                    ("/".to_string(), lc.tr("key-command")),
                    ("Shift+Enter".to_string(), lc.tr("key-newline")),
                    ("Ctrl+T".to_string(), lc.tr("key-switch-model")),
                ]
            };
            format_hints(&hints, key_style, desc_style)
        }
    };

    render_truncated_line(f, left_spans, right_spans, area);
}

/// 将 (key, desc) 对列表格式化为 Span 列表
fn format_hints(
    hints: &[(String, String)],
    key_style: Style,
    desc_style: Style,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span> = Vec::new();
    for (key, desc) in hints {
        spans.push(Span::styled(format!(" {} ", key), key_style));
        spans.push(Span::styled(format!(":{} ", desc), desc_style));
    }
    spans
}

/// 渲染一行 spans，右侧右对齐，超出宽度时截断右侧
fn render_truncated_line(f: &mut Frame, left_spans: Vec<Span>, right_spans: Vec<Span>, area: Rect) {
    let left_width: usize = left_spans.iter().map(|s| s.width()).sum();
    let right_width: usize = right_spans.iter().map(|s| s.width()).sum();

    let total_content_width = left_width + right_width;
    let padding = if total_content_width < area.width as usize {
        " ".repeat(area.width as usize - total_content_width)
    } else {
        " ".to_string()
    };

    let mut all_spans = left_spans;
    all_spans.push(Span::raw(padding));
    all_spans.extend(right_spans);

    f.render_widget(Paragraph::new(Line::from(all_spans)), area);
}
