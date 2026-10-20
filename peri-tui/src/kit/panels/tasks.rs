//! ratatui-kit TasksPanel component.
//!
//! H1g（Iteration 14）：聚合多源任务视图——
//! - 后台任务（从 BG_TASKS atom）
//! - Cron 任务（从 CRON_JOBS atom）
//! - SubAgent 运行时（从 VIEW_MODELS 扫描 TuiSubAgentGroup）
//!
//! 三段拼在同一个连续选中下标空间里（后台段在最前），提供跨调度源的"任务总览"。
//! Cron 任务的 enable/disable/delete 在 Cron 面板、SubAgent 详情在 Agent 面板，
//! 故本面板的行内动作只有「取消后台任务」（选中该行按 Enter，或左键点击该行）。
//!
//! 行结构、可见窗口与鼠标命中由 `viewport` 模块的行表统一推导（绘制、命中、取消目标
//! 解析读同一份结果）。

use std::{collections::HashMap, time::Duration};

use crate::app::panel_types::PanelKind;
use crate::application::operations::{CompletionGate, OperationScope, OperationTicket};
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, ACTIVE_SESSION_ID, BG_TASKS, BRIDGE_RESET_COUNTER, BgTaskEntry, CRON_JOBS,
    CronJobSummary, LANG_VERSION, NOTIFICATION, Notification, VIEW_MODELS,
};
use crate::kit::bg_task_status::BgTaskStatus;
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::panel_mouse::{AreaTracker, is_scrollbar_column};
use crate::kit::panels::shared::list::content_rows;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiSubAgentGroup};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use peri_theme::theme::ThemeDefinition;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Rect},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

mod viewport;
use self::viewport::{
    CancelAction, CancelTarget, SegmentCounts, TaskSegment, TasksRow, TasksViewport,
};

/// 取消请求因无活动会话而未发出时的提示 key。
///
/// [Why] 按面板归属命名（TUI-I18N-001）：文案要表达的是「取消没有发出，因为需要活跃
/// 会话」，而不是 Cron 面板的同类提示，借用会让两处文案互相牵制。
const NO_SESSION_HINT_KEY: &str = "panel-tasks-cancel-no-session";

/// 取消目标已不在列表里时的提示 key——不发请求，且结果必须对用户可见。
const TARGET_GONE_HINT_KEY: &str = "panel-tasks-cancel-target-gone";

type CancelGates = HashMap<String, CompletionGate>;

#[component]
pub fn TasksPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // TUI-HOOK-001：全部 hook 在条件分支与渲染体之前按稳定顺序调用。
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    let cancel_gates = hooks.use_state(CancelGates::default);
    let _ = hooks.use_atom(&ACTIVE_SESSION_ID);
    let _ = hooks.use_atom(&BRIDGE_RESET_COUNTER);
    let cancel_scope = current_scope();
    hooks.use_effect(
        {
            let scope = cancel_scope.clone();
            move || reset_cancel_scope(&mut cancel_gates.write(), scope)
        },
        cancel_scope.clone(),
    );
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);

    // Background Tasks（从 BG_TASKS atom）
    let bg_store = hooks.use_atom(&BG_TASKS);
    let bg_tasks: Vec<BgTaskEntry> = bg_store.read().clone();
    let _ = bg_store;

    // Cron
    let cron_store = hooks.use_atom(&CRON_JOBS);
    let cron_jobs: Vec<CronJobSummary> = cron_store.read().clone();
    let _ = cron_store;

    // SubAgent（从 VIEW_MODELS 扫描）
    let vm_store = hooks.use_atom(&VIEW_MODELS);
    let subagents = collect_subagents(&vm_store.read());
    let _ = vm_store;
    let _ = hooks.use_atom(&LANG_VERSION);

    let counts = SegmentCounts {
        background: bg_tasks.len(),
        cron: cron_jobs.len(),
        subagent: subagents.len(),
    };

    // 面板绘制区域（上一帧）——鼠标行号反推与滚轮仲裁共用这一份区域
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 视口：内容区行数 → 三段行表。绘制（`rows`）、鼠标命中（`hit_item`）与取消目标
    // 解析（`cancel_action`）共用这一份结果，绘制与命中不会各算一遍。
    let (_term_w, term_h) = hooks.use_terminal_size();
    let viewport =
        TasksViewport::for_content_rows(content_rows(area, term_h), counts, *selected.read());

    // 列表刷新（任务完成/取消、SubAgent 数量变化）会让选中下标越界：在 effect 边界收敛
    // 到当帧长度，保证高亮行始终存在（渲染只读状态，不写状态）。
    let selected_now = *selected.read();
    let converged = viewport.selected();
    hooks.use_effect(
        move || *selected.write() = converged,
        (selected_now, converged),
    );

    // 事件上下文随渲染帧重建：取消目标的身份解析读当帧快照，不读按键帧的 atom。
    let event_ctx = TasksEventCtx {
        area,
        selected,
        viewport: viewport.clone(),
        bg_tasks: bg_tasks.clone(),
        cancel_gates,
        cancel_scope,
    };
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| handle_tasks_event(event, &event_ctx),
    );

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel {
        selected,
        area,
        cancel_gates,
    });

    let theme = theme_def.read();
    let sel = viewport.selected();
    let bg_count = counts.background;
    // Enter 仅取消后台任务行；Cron / SubAgent 行的 Enter 关闭面板。提示与动作同判据：
    // 下标空间里后台段在最前，`sel < bg_count` 即「选中行是后台任务行」。
    let nav_hint = if sel < bg_count {
        i18n::tr("panel-tasks-nav-hint")
    } else {
        i18n::tr("panel-tasks-nav-hint-close")
    };

    // 逐行渲染当帧行表：行数、行顺序与行身份都由行表决定，鼠标命中查的是同一张表。
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(viewport.rows().len());
    for row in viewport.rows() {
        lines.push(match *row {
            TasksRow::Summary => summary_line(&theme, counts),
            TasksRow::SectionHeader(segment) => section_header_line(segment, counts, &theme),
            TasksRow::Item(index) => item_line(
                index, counts, &bg_tasks, &cron_jobs, &subagents, sel, &theme,
            ),
            TasksRow::EmptyTitle => Line::from(vec![Span::styled(
                i18n::tr("panel-tasks-empty"),
                Style::new().fg(theme.semantic.text.muted).italic(),
            )]),
            TasksRow::EmptyHintCron => hint_line("panel-tasks-empty-hint-1", &theme),
            TasksRow::EmptyHintSubagent => hint_line("panel-tasks-empty-hint-2", &theme),
            TasksRow::Blank => Line::from(""),
            TasksRow::NavHint => Line::from(nav_hint.clone()).fg(theme.semantic.text.dim),
        });
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入；区域与鼠标命中同源，均为 AreaTracker 结果，
    // 首帧未测量时为 0 尺寸）
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Tasks, area.unwrap_or_default(), sv);

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

/// 事件处理上下文：当帧视口 + 当帧后台任务快照。
///
/// `State<T>` 无公开构造 API，只能从组件挂载取得；`viewport` 与 `bg_tasks` 都来自
/// 渲染这一帧（`use_event_handler` 的闭包每帧重建），因此按键解析出的身份就是用户
/// 看到的那一行——不按键帧重读 atom 按下标取身份。
struct TasksEventCtx {
    area: Option<Rect>,
    selected: State<usize>,
    viewport: TasksViewport,
    bg_tasks: Vec<BgTaskEntry>,
    cancel_gates: State<CancelGates>,
    cancel_scope: OperationScope,
}

/// 面板事件处理——判定集中在这里、组件只做注册，测试可经 `tests::MountedPanel`
/// 捕获真实句柄后直接投喂事件。
fn handle_tasks_event(event: Event, ctx: &TasksEventCtx) -> EventResult {
    // ── 鼠标：区域内左键点击某行 = 选中该行并执行该行的 Enter 语义（click as enter）──
    if let Event::Mouse(mouse) = event {
        if let Some(area) = ctx.area
            && !is_scrollbar_column(&mouse, area)
            && let Some(index) = ctx.viewport.hit_item(&mouse, area)
        {
            *ctx.selected.write() = index;
            activate_row(ctx, index);
            return EventResult::Consumed;
        }
        return match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
            _ => EventResult::Ignored,
        };
    }
    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }
    match key.code {
        KeyCode::Esc => close_panel(),
        KeyCode::Enter => match ctx.viewport.selected_item() {
            Some(index) => activate_row(ctx, index),
            // 列表为空：没有可取消的行，沿用既有语义只关闭面板（提示同样是 Enter::close）
            None => close_panel(),
        },
        // 导航一律基于当帧收敛后的选中值：列表刚刷新过也不会从越界位置起跳
        KeyCode::Up => {
            let mut s = ctx.selected.write();
            *s = previous_selection(ctx.viewport.selected());
        }
        KeyCode::Down => {
            let mut s = ctx.selected.write();
            *s = next_selection(ctx.viewport.selected(), ctx.viewport.counts().total());
        }
        _ => {}
    }
    EventResult::Consumed
}

/// 行动作——键盘 Enter 与鼠标点击共用（click as enter）。
fn activate_row(ctx: &TasksEventCtx, index: usize) {
    match ctx.viewport.cancel_action(index, &ctx.bg_tasks) {
        // 请求真的交给发送侧才关闭面板：没发出去时 dispatch 已提示用户，面板留在原地
        CancelAction::Cancel(target) => {
            if dispatch_cancel(target, ctx.cancel_scope.clone(), ctx.cancel_gates) {
                close_panel();
            }
        }
        // 该行在渲染帧里是可取消的后台任务行，但身份已不在当帧快照里：不发请求，
        // 提示用户并保持面板打开——静默关闭会让用户以为任务已被取消。
        CancelAction::TargetGone => notify(i18n::tr(TARGET_GONE_HINT_KEY)),
        // Cron / SubAgent 行本面板不取消它们，既有语义是 Enter 关闭面板
        CancelAction::NotCancelable => close_panel(),
    }
}

/// 面板发出的取消请求；返回是否交给发送侧（未交给时已给用户可见提示）。
///
/// 会话身份缺失时不发请求：旧实现取 `unwrap_or_default()` 的空串 sessionId，请求必然
/// 被拒，而用户看不到任何原因；这里提示用户并保持面板打开。
fn dispatch_cancel(target: CancelTarget, scope: OperationScope, gates: State<CancelGates>) -> bool {
    if scope.session_id().is_empty()
        || scope != current_scope()
        || active_session_id().as_deref() != Some(scope.session_id())
    {
        tracing::warn!(target: "tasks-panel", "no active session, cancel skipped");
        notify(i18n::tr(NO_SESSION_HINT_KEY));
        return false;
    }
    // 目标有效性由当帧行表的 CancelAction 判定；不能用后来刷新的列表替换用户所见身份。
    let ticket = {
        let mut gates = gates.write();
        let gate = gates.entry(target.task_id.clone()).or_default();
        if gate.is_pending() && gate.scope_matches(&scope) {
            tracing::debug!(task_id = %target.task_id, "duplicate task cancel skipped");
            return false;
        }
        gate.begin(scope)
    };
    send_cancel(ticket, gates, target)
}

fn reset_cancel_scope(gates: &mut CancelGates, scope: OperationScope) {
    gates.retain(|_, gate| !gate.reset_scope(scope.clone()));
}

fn current_scope() -> OperationScope {
    // 测试仍经既有会话接缝注入；生产作用域只取当前前端投影。
    #[cfg(test)]
    if let Some(forced) = tests::forced_session() {
        return OperationScope::new(forced.unwrap_or_default(), BRIDGE_RESET_COUNTER.get());
    }
    OperationScope::new(
        ACTIVE_SESSION_ID.state().read().clone(),
        BRIDGE_RESET_COUNTER.get(),
    )
}

/// 取消请求要带的会话身份；拿不到时为 None。
fn active_session_id() -> Option<String> {
    // 测试接缝：单测进程里 ACP_CLIENT_HANDLE 由其它用例的 OnceLock 决定，构造不出可控
    // 的会话环境，故允许测试注入（`None` = 无活动会话）。生产分支语义不变。
    #[cfg(test)]
    if let Some(forced) = tests::forced_session() {
        return forced;
    }
    ACP_CLIENT_HANDLE.get()?.current_session_id()
}

/// 真正发出取消请求；返回是否交给发送侧。
fn send_cancel(ticket: OperationTicket, gates: State<CancelGates>, target: CancelTarget) -> bool {
    // 测试接缝：测试构建下把请求交给接缝记录并直接返回，不 spawn——真实发送分支只能由
    // 人工 / 端到端验证覆盖（单测进程里 ACP_CLIENT_HANDLE 未必可用）。
    #[cfg(test)]
    if tests::intercept_cancel_request(ticket.scope().session_id(), &target) {
        tests::capture_cancel_ticket(ticket, gates, target);
        return true;
    }
    let Some(client) = ACP_CLIENT_HANDLE.get().cloned() else {
        tracing::warn!(target: "tasks-panel", "ACP_CLIENT_HANDLE not set, cancel skipped");
        gates.write().remove(&target.task_id);
        notify(i18n::tr(NO_SESSION_HINT_KEY));
        return false;
    };
    // 发出前检查身份与目标；已发出的变更仍由 ACP 处理，不因面板关闭撤销。
    if ticket.is_cancelled()
        || ticket.scope() != &current_scope()
        || client.current_session_id().as_deref() != Some(ticket.scope().session_id())
    {
        gates.write().remove(&target.task_id);
        notify(i18n::tr(NO_SESSION_HINT_KEY));
        return false;
    }
    tokio::spawn(async move {
        // spawn 被调度前可能已发生会话重置；此时只能丢弃，不能给新会话提示。
        // 交给发送侧后自动关闭面板不撤销发送，只复检数据作用域。
        if ticket.scope() != &current_scope()
            || client.current_session_id().as_deref() != Some(ticket.scope().session_id())
        {
            tracing::debug!(task_id = %target.task_id, "stale task cancel skipped before send");
            return;
        }
        let result = client
            .cancel_bg_task(ticket.scope().session_id(), &target.task_id)
            .await;
        complete_cancel(gates, &ticket, &target, result);
    });
    true
}

fn complete_cancel(
    gates: State<CancelGates>,
    ticket: &OperationTicket,
    target: &CancelTarget,
    result: Result<serde_json::Value, peri_acp::transport::types::AcpError>,
) -> bool {
    if ticket.scope() != &current_scope() {
        tracing::warn!(task_id = %target.task_id, ?result, "task cancel result dropped: scope changed");
        return false;
    }
    let Some(mut gates) = gates.try_write() else {
        tracing::warn!(task_id = %target.task_id, ?result, "task cancel result dropped: observer gone, outcome unknown");
        return false;
    };
    let Some(gate) = gates.get_mut(&target.task_id) else {
        tracing::debug!(task_id = %target.task_id, "task cancel result dropped: no pending target");
        return false;
    };
    if !gate.accept(ticket) {
        tracing::debug!(task_id = %target.task_id, "task cancel result dropped: stale ticket");
        return false;
    }
    // 保留目标门的代次，旧票不能在后续同目标请求中再次生效。
    let CancelTarget { task_id, kind } = target;
    match result {
        Ok(value) => tracing::info!(
            task_id = %task_id,
            kind = %kind,
            ?value,
            "tasks panel: cancel bg task"
        ),
        Err(error) => {
            tracing::warn!(
                task_id = %task_id,
                kind = %kind,
                %error,
                "tasks panel: cancel bg task failed"
            );
            // 只有完成门接纳的失败才发布提示；关闭面板不等于取消业务成功。
            notify(i18n::tr_args(
                "panel-tasks-cancel-failed",
                &[("detail".to_string(), FluentValue::from(error.to_string()))],
            ));
        }
    }
    true
}

/// 状态栏通知（6s 过期）——取消未能发出的原因必须对用户可见，不能只留日志。
fn notify(message: String) {
    NOTIFICATION.set(Some(Notification {
        message,
        until: peri_time::monotonic_now() + Duration::from_secs(6),
    }));
}

/// 摘要行：任务总数与三段明细。
fn summary_line(theme: &ThemeDefinition, counts: SegmentCounts) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            i18n::tr("panel-tasks-total-label"),
            Style::new().fg(theme.semantic.text.muted),
        ),
        Span::styled(
            format!("{}", counts.total()),
            Style::new().fg(theme.semantic.text.primary).bold(),
        ),
        Span::styled(
            i18n::tr_args(
                "panel-tasks-breakdown",
                &[
                    (
                        "bg".to_string(),
                        FluentValue::from(counts.background as i64),
                    ),
                    ("cron".to_string(), FluentValue::from(counts.cron as i64)),
                    (
                        "subagent".to_string(),
                        FluentValue::from(counts.subagent as i64),
                    ),
                ],
            ),
            Style::new().fg(theme.semantic.text.dim),
        ),
    ])
}

/// 段标题行（含该段项数）。
fn section_header_line(
    segment: TaskSegment,
    counts: SegmentCounts,
    theme: &ThemeDefinition,
) -> Line<'static> {
    let (key, count) = match segment {
        TaskSegment::Background => ("panel-tasks-section-bg", counts.background),
        TaskSegment::Cron => ("panel-tasks-section-cron", counts.cron),
        TaskSegment::SubAgent => ("panel-tasks-section-subagent", counts.subagent),
    };
    Line::from(vec![Span::styled(
        i18n::tr_args(
            key,
            &[("count".to_string(), FluentValue::from(count as i64))],
        ),
        Style::new().fg(theme.semantic.border.active).bold(),
    )])
}

/// 任务行——全量下标与段顺序同源，落到所属段的渲染器。
fn item_line(
    index: usize,
    counts: SegmentCounts,
    bg_tasks: &[BgTaskEntry],
    cron_jobs: &[CronJobSummary],
    subagents: &[TuiSubAgentGroup],
    selected: usize,
    theme: &ThemeDefinition,
) -> Line<'static> {
    let is_selected = index == selected;
    if index < counts.background {
        background_task_line(&bg_tasks[index], is_selected, theme)
    } else if index < counts.background + counts.cron {
        cron_job_line(&cron_jobs[index - counts.background], is_selected, theme)
    } else {
        subagent_line(
            &subagents[index - counts.background - counts.cron],
            is_selected,
            theme,
        )
    }
}

/// 后台任务行：光标、类型标记、任务 id、文本状态标签与详情（shell 任务只显示 pid）。
fn background_task_line(
    task: &BgTaskEntry,
    selected: bool,
    theme: &ThemeDefinition,
) -> Line<'static> {
    let kind_str = match task.kind.as_str() {
        "shell" => i18n::tr("panel-tasks-kind-sh"),
        "agent" => i18n::tr("panel-tasks-kind-ag"),
        "workflow" => i18n::tr("panel-tasks-kind-wf"),
        _ => i18n::tr("panel-tasks-kind-unknown"),
    };
    let status = BgTaskStatus::classify(task.status.as_deref());
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
    Line::from(vec![
        Span::styled(
            cursor_marker(selected),
            Style::new().fg(theme.component.panel.title),
        ),
        Span::styled(kind_str, Style::new().fg(theme.semantic.text.dim)),
        Span::styled(
            format!(" {} ", task.task_id),
            row_name_style(selected, theme),
        ),
        Span::styled(
            format!("[{}] ", status.label()),
            Style::new().fg(bg_task_status_color(status, theme)),
        ),
        Span::styled(detail, Style::new().fg(theme.semantic.text.muted)),
    ])
}

/// 状态标签颜色——仅辅助，文本标签本身独立可读。
fn bg_task_status_color(status: BgTaskStatus, theme: &ThemeDefinition) -> ratatui::style::Color {
    match status {
        BgTaskStatus::Running => theme.semantic.status.running,
        BgTaskStatus::Completed => theme.semantic.status.success,
        BgTaskStatus::Failed | BgTaskStatus::Cancelled => theme.semantic.status.error,
        BgTaskStatus::Lost
        | BgTaskStatus::Reconciling
        | BgTaskStatus::PendingDelivery
        | BgTaskStatus::Unknown => theme.semantic.status.warning,
    }
}

/// Cron 任务行：光标、启用状态、表达式、提示词预览与下次触发时间。
fn cron_job_line(job: &CronJobSummary, selected: bool, theme: &ThemeDefinition) -> Line<'static> {
    let (status_icon, status_color) = if job.enabled {
        ("\u{25cf}", theme.semantic.status.success)
    } else {
        ("\u{25cb}", theme.semantic.text.muted)
    };
    let next_str = job
        .next_fire
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| i18n::tr("common-na"));
    // 按终端显示宽度截断（CJK 双宽按 2 列计），避免字符数截断导致行溢出
    let prompt_preview: String = crate::truncate::truncate_by_width(&job.prompt, 50);
    Line::from(vec![
        Span::styled(
            cursor_marker(selected),
            Style::new().fg(theme.component.panel.title),
        ),
        Span::styled(status_icon, Style::new().fg(status_color)),
        Span::styled(
            format!(" {} ", job.expression),
            Style::new().fg(theme.semantic.text.dim),
        ),
        Span::styled(prompt_preview, row_name_style(selected, theme)),
        Span::styled(
            format!("  @{}", next_str),
            Style::new().fg(theme.semantic.text.muted),
        ),
    ])
}

/// SubAgent 行：光标、名称、agent id、折叠状态与消息数。
fn subagent_line(sa: &TuiSubAgentGroup, selected: bool, theme: &ThemeDefinition) -> Line<'static> {
    let collapsed_marker = if sa.collapsed {
        Span::styled(
            i18n::tr("panel-tasks-collapsed"),
            Style::new().fg(theme.semantic.text.muted),
        )
    } else {
        Span::styled(
            i18n::tr("panel-tasks-live"),
            Style::new().fg(theme.semantic.status.success),
        )
    };
    Line::from(vec![
        Span::styled(
            cursor_marker(selected),
            Style::new().fg(theme.component.panel.title),
        ),
        Span::styled(sa.agent_name.clone(), row_name_style(selected, theme)),
        Span::styled(
            format!("  [{}]", sa.agent_id),
            Style::new().fg(theme.semantic.text.dim),
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
            Style::new().fg(theme.semantic.text.muted),
        ),
    ])
}

/// 行首选中光标（选中行才显示 `>`）。
fn cursor_marker(selected: bool) -> String {
    let cursor = if selected { ">" } else { " " };
    format!(" {} ", cursor)
}

/// 行名样式：选中行高亮加粗。
fn row_name_style(selected: bool, theme: &ThemeDefinition) -> Style {
    if selected {
        Style::new().fg(theme.component.panel.title).bold()
    } else {
        Style::new().fg(theme.semantic.text.primary)
    }
}

/// 空态引导行。
fn hint_line(key: &str, theme: &ThemeDefinition) -> Line<'static> {
    Line::from(vec![Span::styled(
        i18n::tr(key),
        Style::new().fg(theme.semantic.text.dim),
    )])
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
mod tests;
