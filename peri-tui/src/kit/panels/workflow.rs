//! ratatui-kit WorkflowPanel kanban component.
//!
//! Displays workflow run status in a kanban-style layout: run tabs at top,
//! phases on left, agents on right, footer shortcuts at bottom.

use std::time::Duration;

use crate::app::panel_types::PanelKind;
use crate::application::operations::{CompletionGate, OperationScope, OperationTicket};
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, LANG_VERSION, NOTIFICATION,
    Notification, SELECTED_WORKFLOW_RUN_ID, WORKFLOW_SNAPSHOT,
};
use crate::kit::list_nav::{
    cycle_next, cycle_previous, previous_selection, scroll_start_for_selected,
};
use crate::kit::workflow_snapshot::TuiRunProgress;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 终止请求因无活动会话而未发出时的提示 key。
const KILL_NO_SESSION_KEY: &str = "workflow-kill-no-session";

/// 终止请求失败时的提示 key——原始错误进日志，但结果必须对用户可见。
const KILL_FAILED_KEY: &str = "workflow-kill-failed";

/// 目标 run 不在当前快照里时的提示 key（footer 的未同步提示与发送前校验共用）。
const RUN_NOT_SYNCED_KEY: &str = "workflow-run-not-synced";

#[component]
pub fn WorkflowPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // ── hooks：无条件、稳定顺序，全部先于任何条件分支与渲染体（TUI-HOOK-001）──
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let _lang = hooks.use_atom(&LANG_VERSION);
    let snapshot_store = hooks.use_atom(&WORKFLOW_SNAPSHOT);
    let snapshot = snapshot_store.read().clone();
    let _ = snapshot_store;
    // 会话边界投影：终止请求的作用域由本面板构造（原语不读 atoms）；会话 / 前端重置
    // 变化时面板要重渲染，在途请求才会随之收敛
    let _ = hooks.use_atom(&ACTIVE_SESSION_ID);
    let _ = hooks.use_atom(&BRIDGE_RESET_COUNTER);

    // 选中身份：稳定 run id（不是列表下标）——快照重排后仍指向同一条 run
    let selected_run_id = hooks.use_state(|| None::<String>);
    // 已落地的外部选择（`SELECTED_WORKFLOW_RUN_ID` + 当时的 run 列表）：同一组合只应用
    // 一次，用户 Tab / Shift+Tab 切走后不会被外部选择再次拉回
    let preselect_key = hooks.use_state(|| None::<(String, Vec<String>)>);
    let run_sync_missing = hooks.use_state(|| false);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv_phase = hooks.use_state(ScrollViewState::default);
    let sv_agent = hooks.use_state(ScrollViewState::default);
    let focus_left = hooks.use_state(|| true);
    let phase_sel = hooks.use_state(|| 0usize);
    let agent_sel = hooks.use_state(|| 0usize);
    // 终止请求的在途身份（作用域 + 代次 + 取消）由原语持有；响应返回后要过完成门
    // 才准许落地，面板已关闭或会话已切换时结果被丢弃
    let kill_gate = hooks.use_state(CompletionGate::default);

    // 会话边界变化（换会话 / 前端重置）：取消在途等待，此后晚到的响应一律被拒收
    let kill_scope = current_scope();
    hooks.use_effect(
        {
            let scope = kill_scope.clone();
            move || {
                // 同作用域重复投影（每帧 effect）在门内是无操作，不得打断在途请求
                kill_gate.write().reset_scope(scope);
            }
        },
        kill_scope,
    );

    // ── 本帧投影：run 身份列表与选中落点 ────────────────────────────────────
    let runs: &[TuiRunProgress] = snapshot.as_ref().map_or(&[], |snap| snap.runs.as_slice());
    let run_ids: Vec<String> = runs.iter().map(|run| run.run_id.clone()).collect();

    // 选择收敛在快照变更边界（effect）完成，渲染只读状态（TUI-RENDER-001）：
    // 外部选择落点与内部选中身份跟随快照都在这里收敛
    let external_run = SELECTED_WORKFLOW_RUN_ID.state().read().clone();
    hooks.use_effect(
        {
            let external_run = external_run.clone();
            let run_ids = run_ids.clone();
            move || {
                converge_run_selection(
                    selected_run_id,
                    preselect_key,
                    run_sync_missing,
                    external_run.as_deref(),
                    &run_ids,
                )
            }
        },
        (external_run, run_ids.clone()),
    );
    let sel_run = selected_run_index(&run_ids, selected_run_id.read().as_deref());
    let show_run_sync_hint = *run_sync_missing.read();

    // 列内选择（phase / agent）的收敛目标：无 run 时无对象可收敛（占位分支）
    let selected_run = runs.get(sel_run);
    let phase_now = *phase_sel.read();
    let sel_phase = phase_now.min(
        selected_run
            .map_or(0, |run| run.phases.len())
            .saturating_sub(1),
    );
    let sel_phase_title = selected_run
        .and_then(|run| run.phases.get(sel_phase))
        .map(|phase| phase.title.as_str())
        .unwrap_or("");
    let agent_count = match selected_run {
        None => 0,
        Some(run) if sel_phase_title.is_empty() => run.agents.len(),
        Some(run) => run
            .agents
            .iter()
            .filter(|agent| agent.phase.as_deref() == Some(sel_phase_title))
            .count(),
    };
    let agent_now = *agent_sel.read();
    let sel_agent = agent_now.min(agent_count.saturating_sub(1));

    // 越界（快照刚收缩 / 按到列尾）在 effect 边界收敛，渲染只读状态（TUI-RENDER-001）
    hooks.use_effect(
        move || {
            if phase_now != sel_phase {
                *phase_sel.write() = sel_phase;
            }
            if agent_now != sel_agent {
                *agent_sel.write() = sel_agent;
            }
        },
        (phase_now, sel_phase, agent_now, sel_agent),
    );

    // 面板上一帧区域（分隔线高度与滚轮仲裁注册共用）——hook 前置，渲染段不再调用 hook
    let previous_size = hooks.use_previous_size();

    // 事件闭包另持一份 run 身份副本（与渲染端共用同一 atom 快照）
    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        let run_ids_for_events = run_ids.clone();
        move |event| {
            handle_workflow_event(
                event,
                &WorkflowEventCtx {
                    run_ids: &run_ids_for_events,
                    selected_run_id,
                    focus_left,
                    phase_sel,
                    agent_sel,
                    kill_gate,
                },
            )
        }
    });

    #[cfg(test)]
    tests::visit_mounted_panel(tests::MountedPanel {
        selected_run_id,
        focus_left,
        phase_sel,
        agent_sel,
        kill_gate,
    });

    // ── 占位渲染：快照未回填（轮询首帧）/ 本会话无 run ──────────────────────
    if snapshot.is_none() {
        let msg = Paragraph::new(Line::from(vec![Span::styled(
            format!(
                "  {} {}...",
                i18n::tr("common-loading"),
                i18n::tr("workflow-loading-runs")
            ),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        return panel_shell!(PanelKind::Workflow, { Text(text: msg) });
    }

    if runs.is_empty() {
        let msg = Paragraph::new(Line::from(vec![Span::styled(
            format!("  {}", i18n::tr("workflow-no-runs")),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        return panel_shell!(PanelKind::Workflow, { Text(text: msg) });
    }

    // 已判空（上面的占位分支提前返回）：落点由稳定身份推导，必在范围内
    let current_run = &runs[sel_run];

    let focus = *focus_left.read();

    // ── Tab bar ──────────────────────────────────────────────────────────
    let theme = theme_def.read();
    let tab_bar_spans: Vec<Span<'_>> = runs
        .iter()
        .enumerate()
        .map(|(i, run)| {
            let is_selected = i == sel_run;
            let emoji: String = if run.status == "running" {
                running_indicator()
            } else {
                status_emoji_for_run(&run.status).to_string()
            };
            let name = &run.workflow_name;
            let text = format!(" {emoji} {name} ");
            if is_selected {
                Span::styled(
                    text,
                    Style::new()
                        .fg(theme.component.panel.title)
                        .bg(theme.semantic.status.running)
                        .bold(),
                )
            } else {
                Span::styled(text, Style::new().fg(theme.semantic.text.muted))
            }
        })
        .collect();
    let tab_bar = Paragraph::new(Line::from(tab_bar_spans));

    // ── Phase lines ──────────────────────────────────────────────────────
    let mut phase_lines: Vec<Line<'_>> = Vec::new();
    for (pi, phase) in current_run.phases.iter().enumerate() {
        let is_sel = focus && pi == sel_phase;
        let arrow = if is_sel { ">" } else { " " };
        let arrow_style = Style::new().fg(theme.component.panel.title).bold();
        let emoji: String = if phase.status == "active" {
            running_indicator()
        } else {
            status_emoji_for_phase(&phase.status).to_string()
        };
        let emoji_color = phase_status_color(&phase.status, &theme);
        let name = &phase.title;
        let name_style = if is_sel {
            Style::new().fg(theme.component.panel.title).bold()
        } else {
            Style::new().fg(theme.semantic.text.primary)
        };
        let agent_count = current_run
            .agents
            .iter()
            .filter(|a| a.phase.as_deref() == Some(&phase.title))
            .count();
        let agent_tag = if agent_count > 0 {
            format!(" [{agent_count}]")
        } else {
            String::new()
        };
        let tag_style = Style::new().fg(theme.semantic.text.muted);

        phase_lines.push(Line::from(vec![
            Span::styled(arrow, arrow_style),
            Span::styled(format!(" {emoji} "), emoji_color),
            Span::styled(name.chars().take(28).collect::<String>(), name_style),
            Span::styled(agent_tag, tag_style),
        ]));
    }
    // If no phases, show placeholder
    if current_run.phases.is_empty() {
        phase_lines.push(Line::from(vec![Span::styled(
            "  (no phases)",
            Style::new().fg(theme.semantic.text.muted),
        )]));
    }

    // ── Agent lines（按选中 phase 过滤，去除重复 phase 标签）──────────
    let mut agent_lines: Vec<Line<'_>> = Vec::new();
    let filtered_agents: Vec<_> = current_run
        .agents
        .iter()
        .filter(|a| {
            if sel_phase_title.is_empty() {
                true
            } else {
                a.phase.as_deref() == Some(sel_phase_title)
            }
        })
        .collect();
    for (ai, agent) in filtered_agents.iter().enumerate() {
        let is_sel = !focus && ai == sel_agent;
        let arrow = if is_sel { ">" } else { " " };
        let arrow_style = Style::new().fg(theme.component.panel.title).bold();
        let emoji: String = if agent.status == "running" {
            running_indicator()
        } else {
            status_emoji_for_agent(&agent.status).to_string()
        };
        let emoji_color = agent_status_color(&agent.status, &theme);
        let name = agent
            .label
            .as_deref()
            .unwrap_or("?")
            .chars()
            .take(18)
            .collect::<String>();
        // 使用 unicode_width 计算终端列宽，而非 Rust char 计数
        let name_display_width = UnicodeWidthStr::width(name.as_str());
        let name_pad = if name_display_width < 18 {
            " ".repeat(18 - name_display_width)
        } else {
            String::new()
        };
        let tokens_padded = format!("{:>8}", abbreviate_count(agent.token_count.unwrap_or(0)));
        let tools_padded = format!("{:>4}", agent.tool_count.unwrap_or(0));
        let model_cell_text = model_cell(
            agent.model_tier.as_deref().or(agent.model.as_deref()),
            MODEL_COL_WIDTH,
        );
        let name_style = if is_sel {
            Style::new().fg(theme.component.panel.title).bold()
        } else {
            Style::new().fg(theme.semantic.text.primary)
        };
        let dim_style = Style::new().fg(theme.semantic.text.dim);

        agent_lines.push(Line::from(vec![
            Span::styled(arrow, arrow_style),
            Span::styled(format!(" {emoji} "), emoji_color),
            Span::styled(format!("{name}{name_pad}"), name_style),
            Span::styled(format!(" {model_cell_text}"), dim_style),
            Span::styled(format!(" {tokens_padded}"), dim_style),
            Span::styled(format!("  {tools_padded}"), dim_style),
        ]));
    }
    if filtered_agents.is_empty() {
        agent_lines.push(Line::from(vec![Span::styled(
            "  (no agents for this phase)",
            Style::new().fg(theme.semantic.text.muted),
        )]));
    }

    // ── Two-column layout via ratatui View + ScrollView ──────────────────────
    //
    // 左侧 Phase 列 40%，中部 │ 分隔线，右侧 Agents 列 60%。
    // 每列独立 ScrollView，选中项跟随滚动。

    // 各列占满剩余空间，ScrollView 动态裁剪可见项。
    const VISIBLE_ITEMS: usize = 20;

    let phase_scroll = scroll_start_for_selected(sel_phase, phase_lines.len(), VISIBLE_ITEMS);
    let agent_scroll = scroll_start_for_selected(sel_agent, filtered_agents.len(), VISIBLE_ITEMS);

    // Build phases Paragraph: header（含所选 run 总 agent 数）+ visible slice
    let mut phase_text: Vec<Line<'_>> = Vec::new();
    phase_text.push(Line::from(Span::styled(
        format!(
            " {}",
            i18n::tr_args(
                "workflow-phases-header",
                &[(
                    "count".into(),
                    FluentValue::from(current_run.agents.len() as u64),
                )],
            )
        ),
        Style::new().fg(theme.semantic.text.muted).bold(),
    )));
    phase_text.extend(
        phase_lines
            .into_iter()
            .skip(phase_scroll)
            .take(VISIBLE_ITEMS),
    );

    // Build agents Paragraph: header（Model 列与数据行共用列宽常量）+ visible slice
    let mut agent_text: Vec<Line<'_>> = Vec::new();
    let model_header = i18n::tr("workflow-model-header");
    agent_text.push(Line::from(Span::styled(
        agent_table_header(&model_header),
        Style::new().fg(theme.semantic.text.muted).bold(),
    )));
    agent_text.extend(
        agent_lines
            .into_iter()
            .skip(agent_scroll)
            .take(VISIBLE_ITEMS),
    );

    // Divider —— 高度随面板 body 伸缩（固定 30 行会在高面板下断线）
    let divider_style = Style::new().fg(theme.semantic.border.default);
    let body_rows = previous_size.height.saturating_sub(4).max(1) as usize;
    let divider_lines: Vec<Line<'_>> = (0..body_rows)
        .map(|_| Line::from(Span::styled("│", divider_style)))
        .collect();

    let phase_para = Paragraph::new(ratatui::text::Text::from(phase_text));
    let agent_para = Paragraph::new(ratatui::text::Text::from(agent_text));
    let divider_para = Paragraph::new(ratatui::text::Text::from(divider_lines));

    drop(theme);

    // 面板滚轮仲裁注册（双栏：按 40% 切分左右区域，divider 列并入右侧）
    let (phase_area, agent_area) = crate::kit::panel_scroll::split_vertical(previous_size, 40);
    crate::kit::panel_scroll::register_panel_scrolls(
        PanelKind::Workflow,
        vec![
            crate::kit::panel_scroll::PanelScrollSlot {
                area: phase_area,
                state: sv_phase,
            },
            crate::kit::panel_scroll::PanelScrollSlot {
                area: agent_area,
                state: sv_agent,
            },
        ],
    );

    // ── Footer（仅快捷键；四维终态不在面板展示）────────────────────
    let shortcuts_line = i18n::tr("workflow-footer-shortcuts");
    let dim = theme_def.read().semantic.text.dim;
    let mut footer_lines = vec![Line::from(shortcuts_line).style(Style::new().fg(dim))];
    if show_run_sync_hint {
        footer_lines.insert(
            0,
            Line::from(i18n::tr("workflow-run-not-synced")).style(Style::new().fg(dim)),
        );
    }
    let footer = ratatui::text::Text::from(footer_lines);
    let footer_height = if show_run_sync_hint { 2 } else { 1 };

    panel_shell!(PanelKind::Workflow, {
        View(height: Constraint::Length(1)) {
            Text(text: tab_bar)
        }
        View(height: Constraint::Length(1)) {}
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            View(width: Constraint::Percentage(40), height: Constraint::Fill(1)) {
                ScrollView(
                    scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                    state: Some(sv_phase),
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    Text(text: phase_para)
                }
            }
            View(width: Constraint::Length(1), height: Constraint::Fill(1)) {
                Text(text: divider_para)
            }
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                ScrollView(
                    scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                    state: Some(sv_agent),
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    Text(text: agent_para)
                }
            }
        }
        View(height: Constraint::Length(footer_height)) {
            Text(text: Paragraph::new(footer))
        }
    })
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// 本帧选中下标：选中身份命中 → 该 run 的下标；未选中 / 身份不在本帧快照 → 0。
///
/// [Why] 下标只由身份推导，不由上一帧的数值 clamp 而来：快照重排后「下标不动、
/// 指向的 run 变了」正是本面板此前的静默错位。列表为空时返回的 0 是占位值——
/// 渲染与事件判定都在取用前先判空。
fn selected_run_index(run_ids: &[String], selected: Option<&str>) -> usize {
    selected
        .and_then(|id| run_ids.iter().position(|run| run == id))
        .unwrap_or(0)
}

/// 快照变更边界的选择收敛（effect 边界；渲染只读状态——TUI-RENDER-001）。
///
/// 只处理「外部选择」与「内部选中身份跟随快照」两件事，且幂等：同一份
/// `(外部选择, run 列表)` 反复调用不改动任何状态。
fn converge_run_selection(
    selected_run_id: State<Option<String>>,
    preselect_key: State<Option<(String, Vec<String>)>>,
    sync_missing: State<bool>,
    external: Option<&str>,
    run_ids: &[String],
) {
    // 外部选择（`SELECTED_WORKFLOW_RUN_ID`）：同一 (id, 快照) 只应用一次，
    // 用户 Tab / Shift+Tab 切走后不会被再次拉回（既有记忆语义）
    if let Some(external_id) = external {
        let key = (external_id.to_string(), run_ids.to_vec());
        if preselect_key.read().as_ref() == Some(&key) {
            return;
        }
        let found = run_ids.iter().any(|run| run == external_id);
        // 快照里还没有这条 run：先落在首项并提示「未同步」，等轮询把它带回来
        *selected_run_id.write() = if found {
            Some(external_id.to_string())
        } else {
            run_ids.first().cloned()
        };
        *sync_missing.write() = !found;
        *preselect_key.write() = Some(key);
        return;
    }

    // 外部选择已撤销：清掉记忆，否则同样的外部选择再次出现会被误判为「已应用」
    if preselect_key.read().is_some() {
        *preselect_key.write() = None;
        *sync_missing.write() = false;
    }

    // 内部选中身份跟随快照：id 消失（run 被淘汰 / 快照收缩）才 clamp 到首项，
    // 首帧尚未选中时也落到首项——选中身份因此始终是快照里的真实 run
    let current = selected_run_id.read().clone();
    let valid = current
        .as_deref()
        .is_some_and(|id| run_ids.iter().any(|run| run == id));
    if !valid {
        *selected_run_id.write() = run_ids.first().cloned();
    }
}

// ── 键盘事件判定 ────────────────────────────────────────────────────────

/// 事件判定所需的当帧投影：run 身份列表 + 面板句柄。
///
/// `State<T>` 无公开构造 API，只能从组件挂载取得；集中成一个结构体，事件入口签名
/// 不再随状态增长。`run_ids` 是渲染该事件闭包的那一帧的列表（生产里闭包随渲染帧
/// 重建；测试经挂载探针喂事件时模拟同一份投影）。
struct WorkflowEventCtx<'a> {
    run_ids: &'a [String],
    /// 面板选中身份（稳定 run id）
    selected_run_id: State<Option<String>>,
    focus_left: State<bool>,
    phase_sel: State<usize>,
    agent_sel: State<usize>,
    /// 终止请求的在途身份（作用域 + 代次 + 取消）
    kill_gate: State<CompletionGate>,
}

/// 面板事件处理——判定集中在这里，组件只做注册；测试可经挂载探针取真实句柄后直接投喂。
fn handle_workflow_event(event: Event, ctx: &WorkflowEventCtx<'_>) -> EventResult {
    let Event::Key(key) = event else {
        return EventResult::Ignored;
    };
    if key.kind != KeyEventKind::Press {
        return EventResult::Ignored;
    }
    // 无 run 时面板只有占位文案：按键沿用「面板未注册 handler」的既有语义（不消费）
    if ctx.run_ids.is_empty() {
        return EventResult::Ignored;
    }
    match key.code {
        KeyCode::Esc => {
            close_panel();
            EventResult::Consumed
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let current = selected_run_index(ctx.run_ids, ctx.selected_run_id.read().as_deref());
            let next = if key.code == KeyCode::Tab {
                cycle_next(current, ctx.run_ids.len())
            } else {
                cycle_previous(current, ctx.run_ids.len())
            };
            // 切 run 时列内选择回到首项（既有语义）
            if let Some(run_id) = ctx.run_ids.get(next) {
                *ctx.selected_run_id.write() = Some(run_id.clone());
            }
            *ctx.phase_sel.write() = 0;
            *ctx.agent_sel.write() = 0;
            EventResult::Consumed
        }
        KeyCode::Left => {
            *ctx.focus_left.write() = true;
            EventResult::Consumed
        }
        KeyCode::Right => {
            *ctx.focus_left.write() = false;
            EventResult::Consumed
        }
        KeyCode::Up => {
            if *ctx.focus_left.read() {
                let mut phase = ctx.phase_sel.write();
                *phase = previous_selection(*phase);
            } else {
                let mut agent = ctx.agent_sel.write();
                *agent = previous_selection(*agent);
            }
            EventResult::Consumed
        }
        KeyCode::Down => {
            if *ctx.focus_left.read() {
                let mut phase = ctx.phase_sel.write();
                *phase = phase.saturating_add(1);
            } else {
                let mut agent = ctx.agent_sel.write();
                *agent = agent.saturating_add(1);
            }
            EventResult::Consumed
        }
        KeyCode::Enter => {
            // 终止当前选中的 run（workflow/kill_run）：目标是面板选中身份（稳定 run id），
            // 不按旧下标取「现在这一项」。面板保持打开——轮询 workflow/list_runs 会显示
            // killed 状态。
            if let Some(run_id) = ctx.selected_run_id.read().clone() {
                dispatch_kill(run_id, ctx.kill_gate);
            }
            EventResult::Consumed
        }
        _ => EventResult::Ignored,
    }
}

// ── 终止运行（workflow/kill_run）─────────────────────────────────────────

/// 当前会话边界的操作作用域：活动会话 + 前端重置代次。
///
/// 面板是会话投影的消费者，取值留在这一侧；`application::operations` 只接受调用方
/// 构造好的作用域，自己不读 atoms。
fn current_scope() -> OperationScope {
    OperationScope::new(
        ACTIVE_SESSION_ID.state().read().clone(),
        BRIDGE_RESET_COUNTER.get(),
    )
}

/// 目标 run 是否仍在当前快照中——发送前的身份校验。
fn snapshot_contains_run(run_id: &str) -> bool {
    WORKFLOW_SNAPSHOT
        .state()
        .read()
        .as_ref()
        .is_some_and(|snapshot| snapshot.runs.iter().any(|run| run.run_id == run_id))
}

/// 发起终止请求；返回是否交给发送侧（未交给时已给用户可见提示）。
///
/// [Why] 旧实现按渲染期的下标取 run，并把「无会话」取成空串 sessionId 发出去：
/// 前者的身份会随快照重排漂移，后者必然被拒而用户看不到原因。
fn dispatch_kill(run_id: String, kill_gate: State<CompletionGate>) -> bool {
    let scope = current_scope();
    if scope.session_id().is_empty() {
        tracing::warn!(run_id = %run_id, "workflow panel: no active session, kill skipped");
        notify(i18n::tr(KILL_NO_SESSION_KEY));
        return false;
    }
    if !snapshot_contains_run(&run_id) {
        tracing::warn!(run_id = %run_id, "workflow panel: run not in snapshot, kill skipped");
        notify(i18n::tr(RUN_NOT_SYNCED_KEY));
        return false;
    }
    let ticket = kill_gate.write().begin(scope);
    send_kill(ticket, kill_gate, run_id)
}

/// 真正发出终止请求；请求参数里的会话身份取自作用域（与完成门同一份边界身份）。
fn send_kill(ticket: OperationTicket, kill_gate: State<CompletionGate>, run_id: String) -> bool {
    // 测试接缝：测试构建下把请求交给接缝记录并直接返回，不 spawn——真实发送分支只能由
    // 人工 / 端到端验证覆盖（单测进程里 ACP_CLIENT_HANDLE 未必可用）。
    #[cfg(test)]
    if tests::intercept_kill_request(ticket.scope().session_id(), &run_id) {
        return true;
    }
    let Some(client) = ACP_CLIENT_HANDLE.get().cloned() else {
        // 请求没发出去：在途身份随之失效，完成门不会停在「等待中」
        kill_gate.write().cancel();
        tracing::warn!(run_id = %run_id, "workflow panel: ACP_CLIENT_HANDLE not set, kill skipped");
        notify(i18n::tr(KILL_NO_SESSION_KEY));
        return false;
    };
    let session_id = ticket.scope().session_id().to_string();
    tokio::spawn(async move {
        let result = client.kill_workflow_run(&session_id, &run_id).await;
        // 完成门：面板已关闭（句柄随组件卸载失效）或会话已切换（作用域不符）的响应
        // 在这里被丢弃，不写回任何状态
        let Some(mut gate) = kill_gate.try_write() else {
            tracing::warn!(run_id = %run_id, "workflow panel: result dropped, panel gone");
            return;
        };
        if !gate.accept(&ticket) {
            tracing::warn!(run_id = %run_id, "workflow panel: result dropped, stale request");
            return;
        }
        match result {
            // 「已请求终止」是诚实表述：服务端可能仍在收尾
            Ok(value) => {
                tracing::info!(run_id = %run_id, ?value, "workflow panel: kill_run requested");
                notify(i18n::tr_args(
                    "workflow-kill-requested",
                    &[("run".to_string(), FluentValue::from(run_id))],
                ));
            }
            Err(error) => {
                tracing::warn!(run_id = %run_id, %error, "workflow panel: kill_run failed");
                notify(i18n::tr(KILL_FAILED_KEY));
            }
        }
    });
    true
}

/// 状态栏通知（2s 过期）——终止请求的结果必须对用户可见，不能只留日志。
fn notify(message: String) {
    *NOTIFICATION.state().write() = Some(Notification {
        message,
        until: peri_time::monotonic_now() + Duration::from_secs(2),
    });
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

fn status_emoji_for_run(status: &str) -> &'static str {
    match status {
        "running" => "\u{25cf}",           // ●
        "completed" => "\u{2713}",         // ✓
        "failed" | "killed" => "\u{2717}", // ✗
        _ => "\u{25cb}",                   // ○
    }
}

fn status_emoji_for_phase(status: &str) -> &'static str {
    match status {
        "active" => "\u{25cf}",  // ●
        "done" => "\u{2713}",    // ✓
        "pending" => "\u{25cb}", // ○
        _ => "\u{25cb}",         // ○
    }
}

fn status_emoji_for_agent(status: &str) -> &'static str {
    match status {
        "running" => "\u{25cf}",          // ●
        "done" => "\u{2713}",             // ✓
        "pending" => "\u{25cb}",          // ○
        "dead" | "skipped" => "\u{2717}", // ✗
        _ => "\u{25cb}",                  // ○
    }
}

fn phase_status_color(
    status: &str,
    theme: &peri_theme::theme::ThemeDefinition,
) -> ratatui::style::Style {
    Style::new().fg(match status {
        "active" => theme.semantic.status.running,
        "done" => theme.semantic.status.success,
        "failed" => theme.semantic.status.error,
        _ => theme.semantic.text.muted,
    })
}

fn agent_status_color(
    status: &str,
    theme: &peri_theme::theme::ThemeDefinition,
) -> ratatui::style::Style {
    Style::new().fg(match status {
        "running" => theme.semantic.status.running,
        "done" => theme.semantic.status.success,
        "dead" | "skipped" => theme.semantic.status.error,
        _ => theme.semantic.text.muted,
    })
}

/// 壁钟驱动的运行中动画帧指示器。每 100ms 推进一帧。
fn running_indicator() -> String {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    const FRAMES: &[char] = &[
        '✳', '✴', '✵', '✶', '✷', '✸', '✹', '✺', '✻', '✼', '❃', '❊', '✼', '✻', '✺', '✸',
    ];
    let start = START.get_or_init(peri_time::monotonic_now);
    let tick = (peri_time::elapsed_since(*start).as_millis() / 100) as usize;
    let frame = FRAMES[tick % FRAMES.len()];
    format!("{frame}")
}

/// Abbreviate a count into a human-readable short form.
fn abbreviate_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{n}")
    }
}

/// Model 列显示宽度（终端列）。
const MODEL_COL_WIDTH: usize = 12;

/// Agent 名列显示宽度（与行内 `take(18)` / padding 一致）。
const AGENT_NAME_COL_WIDTH: usize = 18;

/// 数据行在 name 列之前的终端列数（`>` + ` {emoji} `）。
const AGENT_ROW_LEADING_COLS: usize = 4;

/// Agents 表头：列位置与数据行 `arrow + emoji + name` 对齐。
fn agent_table_header(model_header: &str) -> String {
    let mut out = String::from(' ');
    out.push_str("Agents");
    let used = 1 + UnicodeWidthStr::width("Agents");
    let pad_to_model = AGENT_ROW_LEADING_COLS + AGENT_NAME_COL_WIDTH;
    if pad_to_model > used {
        out.push_str(&" ".repeat(pad_to_model - used));
    }
    out.push_str(&model_cell(Some(model_header), MODEL_COL_WIDTH));
    out.push_str("  Tokens    Tools");
    out
}

/// Model 列单元格：缺失显示 '-'；超过列宽按显示宽度截断（Unicode 安全），
/// 未超过则补齐到列宽。
fn model_cell(model: Option<&str>, width: usize) -> String {
    let s = model.unwrap_or("-");
    let w = UnicodeWidthStr::width(s);
    if w <= width {
        format!("{s}{}", " ".repeat(width - w))
    } else {
        truncate_to_width(s, width)
    }
}

/// 按终端显示宽度截断（Unicode 字符边界安全）：显示宽度超过 `width` 时
/// 保留 `width - 1` 列并追加 '…'。
fn truncate_to_width(s: &str, width: usize) -> String {
    if UnicodeWidthStr::width(s) <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > keep {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
#[path = "workflow_test.rs"]
mod tests;
