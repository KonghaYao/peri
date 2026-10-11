//! ratatui-kit TasksPanel component.
//!
//! H1g（Iteration 14）：聚合多源任务视图——
//! - Cron 任务（从 CRON_JOBS atom）
//! - SubAgent 运行时（从 VIEW_MODELS 扫描 TuiSubAgentGroup）
//!
//! 只读面板。Cron 任务的 enable/disable/delete 在 Cron 面板；SubAgent 详情
//! 在 Agent 面板。本面板提供跨调度源的"任务总览"。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{ACP_CLIENT_HANDLE, BG_TASKS, CRON_JOBS, LANG_VERSION, VIEW_MODELS};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiSubAgentGroup};
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

#[component]
pub fn TasksPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);

    // Background Tasks（从 BG_TASKS atom）
    let bg_store = hooks.use_atom(&BG_TASKS);
    let bg_tasks = bg_store.read().clone();
    let _ = bg_store;

    // Cron
    let cron_store = hooks.use_atom(&CRON_JOBS);
    let cron_jobs = cron_store.read().clone();
    let _ = cron_store;

    // SubAgent（从 VIEW_MODELS 扫描）
    let vm_store = hooks.use_atom(&VIEW_MODELS);
    let subagents = collect_subagents(&vm_store.read());
    let _ = vm_store;
    let _ = hooks.use_atom(&LANG_VERSION);

    let bg_count = bg_tasks.len();
    let cron_count = cron_jobs.len();
    let subagent_count = subagents.len();
    let total = bg_count + cron_count + subagent_count;

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            match key.code {
                KeyCode::Esc => close_panel(),
                KeyCode::Enter => {
                    // All session task handles use the same ACP cancellation route.
                    let sel = *selected.read();
                    let bg_count = BG_TASKS.state().read().len();
                    if sel < bg_count
                        && let Some(task) = BG_TASKS.state().read().get(sel)
                    {
                        let task_id = task.task_id.clone();
                        let kind = task.kind.clone();
                        if let Some(client) = ACP_CLIENT_HANDLE.get() {
                            let client = client.clone();
                            let sid = client.current_session_id().unwrap_or_default();
                            tokio::spawn(async move {
                                let result = client.cancel_bg_task(&sid, &task_id).await;
                                match result {
                                    Ok(v) => tracing::info!(
                                        task_id = %task_id,
                                        kind = %kind,
                                        ?v,
                                        "tasks panel: cancel bg task"
                                    ),
                                    Err(e) => tracing::warn!(
                                        task_id = %task_id,
                                        kind = %kind,
                                        error = %e,
                                        "tasks panel: cancel bg task failed"
                                    ),
                                }
                            });
                        } else {
                            tracing::warn!(target: "tasks-panel", "ACP_CLIENT_HANDLE not set, cancel skipped");
                        }
                    }
                    close_panel()
                }
                KeyCode::Up => {
                    let mut s = selected.write();
                    *s = previous_selection(*s);
                }
                KeyCode::Down => {
                    let mut s = selected.write();
                    let total = BG_TASKS.state().read().len()
                        + CRON_JOBS.state().read().len()
                        + collect_subagents(&VIEW_MODELS.state().read()).len();
                    if total > 0 {
                        *s = next_selection(*s, total);
                    }
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 摘要
    lines.push(Line::from(vec![
        Span::styled(
            i18n::tr("panel-tasks-total-label"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        ),
        Span::styled(
            format!("{}", total),
            Style::new()
                .fg(theme_def.read().semantic.text.primary)
                .bold(),
        ),
        Span::styled(
            i18n::tr_args(
                "panel-tasks-breakdown",
                &[
                    ("bg".to_string(), FluentValue::from(bg_count as i64)),
                    ("cron".to_string(), FluentValue::from(cron_count as i64)),
                    (
                        "subagent".to_string(),
                        FluentValue::from(subagent_count as i64),
                    ),
                ],
            ),
            Style::new().fg(theme_def.read().semantic.text.dim),
        ),
    ]));
    lines.push(Line::from(""));

    // Background Tasks section
    if !bg_tasks.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr_args(
                "panel-tasks-section-bg",
                &[("count".to_string(), FluentValue::from(bg_count as i64))],
            ),
            Style::new()
                .fg(theme_def.read().semantic.border.active)
                .bold(),
        )]));
        for (i, task) in bg_tasks.iter().enumerate() {
            let row_idx = i;
            let is_selected = row_idx == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };
            let kind_str = match task.kind.as_str() {
                "shell" => i18n::tr("panel-tasks-kind-sh"),
                "agent" => i18n::tr("panel-tasks-kind-ag"),
                "workflow" => i18n::tr("panel-tasks-kind-wf"),
                _ => i18n::tr("panel-tasks-kind-unknown"),
            };
            // shell 任务的 summary 是脚本本身，无展示意义 → 只显示 pid
            let is_shell = task.kind.as_str() == "shell";
            let pid_str = task
                .pid
                .map(|p| {
                    i18n::tr_args(
                        "panel-tasks-pid",
                        &[("pid".to_string(), FluentValue::from(p as i64))],
                    )
                })
                .unwrap_or_default();
            // 按终端显示宽度截断（CJK 双宽按 2 列计），避免字符数截断导致行溢出
            let detail = if is_shell {
                pid_str
            } else {
                format!(
                    "{}{}",
                    crate::truncate::truncate_by_width(&task.summary, 60),
                    pid_str
                )
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(
                    kind_str,
                    Style::new().fg(theme_def.read().semantic.text.dim),
                ),
                Span::styled(format!(" {} ", task.task_id), name_style),
                Span::styled(
                    detail,
                    Style::new().fg(theme_def.read().semantic.text.muted),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Cron section
    if !cron_jobs.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr_args(
                "panel-tasks-section-cron",
                &[("count".to_string(), FluentValue::from(cron_count as i64))],
            ),
            Style::new()
                .fg(theme_def.read().semantic.border.active)
                .bold(),
        )]));
        for (i, job) in cron_jobs.iter().enumerate() {
            let row_idx = bg_count + i;
            let is_selected = row_idx == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };
            let (status_icon, status_color) = if job.enabled {
                ("\u{25cf}", theme_def.read().semantic.status.success)
            } else {
                ("\u{25cb}", theme_def.read().semantic.text.muted)
            };
            let next_str = job
                .next_fire
                .map(|t| t.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| i18n::tr("common-na"));

            // 按终端显示宽度截断（CJK 双宽按 2 列计），避免字符数截断导致行溢出
            let prompt_preview: String = crate::truncate::truncate_by_width(&job.prompt, 50);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(status_icon, Style::new().fg(status_color)),
                Span::styled(
                    format!(" {} ", job.expression),
                    Style::new().fg(theme_def.read().semantic.text.dim),
                ),
                Span::styled(prompt_preview, name_style),
                Span::styled(
                    format!("  @{}", next_str),
                    Style::new().fg(theme_def.read().semantic.text.muted),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    // SubAgent section
    if !subagents.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr_args(
                "panel-tasks-section-subagent",
                &[(
                    "count".to_string(),
                    FluentValue::from(subagent_count as i64),
                )],
            ),
            Style::new()
                .fg(theme_def.read().semantic.border.active)
                .bold(),
        )]));
        for (i, sa) in subagents.iter().enumerate() {
            let row_idx = bg_count + cron_count + i;
            let is_selected = row_idx == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };
            let collapsed_marker = if sa.collapsed {
                Span::styled(
                    i18n::tr("panel-tasks-collapsed"),
                    Style::new().fg(theme_def.read().semantic.text.muted),
                )
            } else {
                Span::styled(
                    i18n::tr("panel-tasks-live"),
                    Style::new().fg(theme_def.read().semantic.status.success),
                )
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(sa.agent_name.clone(), name_style),
                Span::styled(
                    format!("  [{}]", sa.agent_id),
                    Style::new().fg(theme_def.read().semantic.text.dim),
                ),
                collapsed_marker,
                Span::styled(
                    i18n::tr_args(
                        "panel-tasks-msgs",
                        &[(
                            "count".to_string(),
                            FluentValue::from(sa.view_models.len() as i64),
                        )],
                    ),
                    Style::new().fg(theme_def.read().semantic.text.muted),
                ),
            ]));
        }
    }

    if total == 0 {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-tasks-empty"),
            Style::new()
                .fg(theme_def.read().semantic.text.muted)
                .italic(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-tasks-empty-hint-1"),
            Style::new().fg(theme_def.read().semantic.text.dim),
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-tasks-empty-hint-2"),
            Style::new().fg(theme_def.read().semantic.text.dim),
        )]));
    }

    lines.push(Line::from(""));
    // Enter 仅在选中后台任务行时取消该任务；Cron / SubAgent 行 Enter 只关闭面板。
    let nav_hint = if sel < bg_count {
        i18n::tr("panel-tasks-nav-hint")
    } else {
        i18n::tr("panel-tasks-nav-hint-close")
    };
    lines.push(Line::from(nav_hint).fg(theme_def.read().semantic.text.dim));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入，area 用上一帧组件区域）
    crate::kit::panel_scroll::register_panel_scroll(
        PanelKind::Tasks,
        hooks.use_previous_size(),
        sv,
    );

    panel_shell!(PanelKind::Tasks, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                state: Some(sv),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
}

/// 从 ViewModelsSnapshot 派生去重 SubAgent 列表。
fn collect_subagents(snap: &crate::kit::atoms::ViewModelsSnapshot) -> Vec<TuiSubAgentGroup> {
    let mut out: Vec<TuiSubAgentGroup> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for vm in snap.items.iter() {
        scan_vm_for_subagents(vm, &mut out, &mut seen);
    }
    out
}

fn scan_vm_for_subagents(
    vm: &TuiRenderUnit,
    out: &mut Vec<TuiSubAgentGroup>,
    seen: &mut std::collections::HashSet<String>,
) {
    if let TuiRenderUnit::TuiSubAgentGroup(d) = vm {
        if seen.insert(d.agent_id.clone()) {
            out.push(d.clone());
        }
        for child in d.view_models.iter() {
            scan_vm_for_subagents(child, out, seen);
        }
    } else if let TuiRenderUnit::TuiCollapsedGroup(g) = vm {
        for child in g.view_models.iter() {
            scan_vm_for_subagents(child, out, seen);
        }
    }
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_tasks_enter_still_cancels_not_open_detail() {
        let src = include_str!("tasks.rs");
        let main_src = src.split("#[cfg(test)]").next().expect("tasks module body");
        assert!(
            main_src.contains("cancel_bg_task") || main_src.contains("kill_workflow_run"),
            "Tasks Enter 应取消 bg 任务"
        );
        assert!(
            !main_src.contains("open_panel"),
            "Tasks 面板 Enter 不得打开 SubAgent/Shell 详情抽屉"
        );
    }

    /// P0-74：底部导航提示必须描述真实动作——后台任务行 Enter 取消，Cron / SubAgent 行 Enter 只关闭。
    #[test]
    fn test_tasks_nav_hint_describes_cancel_not_open() {
        let src = include_str!("tasks.rs");
        let main_src = src.split("#[cfg(test)]").next().expect("tasks module body");
        assert!(
            !main_src.contains("common-nav-enter-close"),
            "Tasks 面板不得复用 Enter::open 语义的通用提示"
        );

        // 动作侧：Enter 只在选中后台任务行时取消（sel < bg_count），其余行走 close_panel
        let enter_arm = main_src
            .split("KeyCode::Enter =>")
            .nth(1)
            .and_then(|rest| rest.split("KeyCode::Up =>").next())
            .expect("Tasks Enter 分支存在");
        assert!(
            enter_arm.contains("sel < bg_count") && enter_arm.contains("cancel_bg_task"),
            "Tasks Enter 的真实动作是仅对后台任务行取消；动作语义变化须同步提示与该用例"
        );

        // 提示侧：按选中行落在哪个分组选择 key
        let hint_branch = main_src
            .split("let nav_hint = if sel < bg_count")
            .nth(1)
            .and_then(|rest| rest.split("lines.push(Line::from(nav_hint))").next())
            .expect("Tasks 提示选择分支存在");
        assert!(
            hint_branch.contains(r#"i18n::tr("panel-tasks-nav-hint")"#),
            "后台任务行提示须使用 Tasks 取消专属 key"
        );
        assert!(
            hint_branch.contains(r#"i18n::tr("panel-tasks-nav-hint-close")"#),
            "Cron / SubAgent 行提示须使用关闭专属 key"
        );

        for lang in ["en", "zh-CN"] {
            let reg = crate::i18n::LcRegistry::new(Some(lang));
            let cancel = reg.tr("panel-tasks-nav-hint");
            assert!(
                !cancel.contains("Enter::open"),
                "{lang} 提示不得把取消伪装成打开：{cancel}"
            );
            assert!(
                cancel.contains("Enter::cancel") || cancel.contains("Enter::取消"),
                "{lang} 提示须标明 Enter 取消：{cancel}"
            );

            let close = reg.tr("panel-tasks-nav-hint-close");
            assert!(
                !close.contains("Enter::open"),
                "{lang} 提示不得把关闭伪装成打开：{close}"
            );
            assert!(
                close.contains("Enter::close") || close.contains("Enter::关闭"),
                "{lang} 非后台任务行提示须标明 Enter 关闭：{close}"
            );
            assert!(
                !close.contains("Enter::cancel") && !close.contains("Enter::取消"),
                "{lang} 非后台任务行 Enter 不取消，提示不得声称取消：{close}"
            );
        }
    }
}
