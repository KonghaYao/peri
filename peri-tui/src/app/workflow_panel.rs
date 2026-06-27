//! WorkflowPanel — 三级树展示 workflow 执行进度（Run → Phase → Agent）
//!
//! GAP-08: 从扁平列表改为三级树布局——顶部 TabBar 切换 Run，
//! 下方展示选中 Run 的 Phase/Agent 树。

use std::any::Any;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};
use tui_textarea::Input;

use super::{
    panel_component::PanelComponent,
    panel_manager::{EventResult, PanelContext, PanelKind},
    App,
};

/// Workflow 进度快照条目（从 WorkflowProgressStore 拷贝）
#[derive(Debug, Clone)]
pub struct WorkflowRunSnapshot {
    pub run_id: String,
    pub workflow_name: String,
    pub status: String,
    pub phases: Vec<WorkflowPhaseSnapshot>,
    pub agents: Vec<WorkflowAgentSnapshot>,
}

#[derive(Debug, Clone)]
pub struct WorkflowPhaseSnapshot {
    pub title: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct WorkflowAgentSnapshot {
    pub agent_id: u64,
    pub label: Option<String>,
    pub phase: Option<String>,
    pub status: String,
    pub token_count: Option<u64>,
    pub tool_count: Option<u64>,
}

/// 焦点区域——Tab 切换时保持当前列表的导航位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusZone {
    Phases,
    Agents,
}

/// Workflow 面板——三级树布局（Run tab + Phase/Agent 分栏）
pub struct WorkflowPanel {
    runs: Vec<WorkflowRunSnapshot>,
    /// 当前选中的 Run tab 索引。
    selected_run: usize,
    /// Phase 列表导航。
    phase_cursor: usize,
    phase_state: ListState,
    /// Agent 列表导航。
    agent_cursor: usize,
    agent_state: ListState,
    /// 当前焦点（Phases 或 Agents）。
    focus: FocusZone,
    /// 预计算的 Tab label（在 update_runs 时计算，避免 render 每帧 format!）。
    cached_tab_labels: Vec<String>,
}

impl WorkflowPanel {
    pub fn new(runs: Vec<WorkflowRunSnapshot>) -> Self {
        let tab_labels = Self::build_tab_labels(&runs);
        Self {
            runs,
            selected_run: 0,
            phase_cursor: 0,
            phase_state: ListState::default(),
            agent_cursor: 0,
            agent_state: ListState::default(),
            focus: FocusZone::Agents,
            cached_tab_labels: tab_labels,
        }
    }

    /// 构建 Tab label 字符串（预计算，避免 render 每帧 format! 分配）。
    fn build_tab_labels(runs: &[WorkflowRunSnapshot]) -> Vec<String> {
        runs.iter()
            .map(|r| {
                let icon = Self::status_icon(&r.status);
                let short_id = &r.run_id[..8.min(r.run_id.len())];
                format!(" {icon} {} [{short_id}] ", r.workflow_name)
            })
            .collect()
    }

    /// 更新面板数据（用于实时刷新）。
    pub fn update_runs(&mut self, runs: Vec<WorkflowRunSnapshot>) {
        self.cached_tab_labels = Self::build_tab_labels(&runs);
        self.runs = runs;
        if self.selected_run >= self.runs.len() && !self.runs.is_empty() {
            self.selected_run = self.runs.len().saturating_sub(1);
        }
        self.clamp_cursors();
    }

    fn clamp_cursors(&mut self) {
        let phase_count = match self.selected_run_data() {
            Some(run) => run.phases.len(),
            None => return,
        };
        if self.phase_cursor >= phase_count && phase_count > 0 {
            self.phase_cursor = phase_count - 1;
        }
        let agent_count = self.filtered_agent_count();
        if self.agent_cursor >= agent_count && agent_count > 0 {
            self.agent_cursor = agent_count - 1;
        }
    }

    fn selected_run_data(&self) -> Option<&WorkflowRunSnapshot> {
        self.runs.get(self.selected_run)
    }

    /// 按当前选中 phase 筛选后的 agent 数量。
    /// run 无 phases 时返回全部 agent 数（此时右侧 agents 区不过滤）。
    fn filtered_agent_count(&self) -> usize {
        let run = match self.selected_run_data() {
            Some(r) => r,
            None => return 0,
        };
        if run.phases.is_empty() {
            return run.agents.len();
        }
        let title = run.phases.get(self.phase_cursor).map(|p| p.title.as_str());
        run.agents
            .iter()
            .filter(|a| a.phase.as_deref() == title)
            .count()
    }

    fn next_run(&mut self) {
        if !self.runs.is_empty() {
            self.selected_run = (self.selected_run + 1) % self.runs.len();
            self.phase_cursor = 0;
            self.agent_cursor = 0;
        }
    }

    fn move_phase_cursor(&mut self, delta: i32) {
        let count = self
            .selected_run_data()
            .map(|r| r.phases.len())
            .unwrap_or(0);
        if count == 0 {
            return;
        }
        self.phase_cursor = (self.phase_cursor as i32 + delta)
            .max(0)
            .min(count as i32 - 1) as usize;
        // Phase 切换后右侧 agent 列表重新筛选，光标回到首项避免越界/错位
        self.agent_cursor = 0;
    }

    fn move_agent_cursor(&mut self, delta: i32) {
        let count = self.filtered_agent_count();
        if count == 0 {
            return;
        }
        self.agent_cursor = (self.agent_cursor as i32 + delta)
            .max(0)
            .min(count as i32 - 1) as usize;
    }

    /// 当前选中的 agent（用于 kill 操作）。
    /// 索引基于 `filtered_agent_count` 对应的筛选序列，而非 run.agents 原始顺序。
    pub fn selected_agent(&self) -> Option<(String, u64)> {
        let run = self.selected_run_data()?;
        let agent = if run.phases.is_empty() {
            run.agents.get(self.agent_cursor)
        } else {
            let title = run.phases.get(self.phase_cursor).map(|p| p.title.as_str());
            run.agents
                .iter()
                .filter(|a| a.phase.as_deref() == title)
                .nth(self.agent_cursor)
        }?;
        Some((run.run_id.clone(), agent.agent_id))
    }

    /// 当前选中的 run_id（用于 kill workflow 操作）。
    pub fn selected_run_id(&self) -> Option<String> {
        self.selected_run_data().map(|r| r.run_id.clone())
    }

    fn status_color(status: &str) -> Color {
        match status {
            "running" | "active" => Color::Yellow,
            "completed" | "done" => Color::Green,
            "failed" | "dead" => Color::Red,
            "killed" => Color::Magenta,
            "skipped" => Color::DarkGray,
            "pending" => Color::Gray,
            _ => Color::White,
        }
    }

    fn status_icon(status: &str) -> &'static str {
        match status {
            "running" | "active" => "▶",
            "completed" | "done" => "✓",
            "failed" | "dead" => "✗",
            "killed" => "☠",
            "skipped" => "⊘",
            "pending" => "○",
            _ => "•",
        }
    }
}

impl PanelComponent for WorkflowPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Workflow
    }

    fn handle_key(&mut self, input: Input, _ctx: &mut PanelContext<'_>) -> EventResult {
        use tui_textarea::Key;

        match input {
            Input { key: Key::Esc, .. }
            | Input {
                key: Key::Char('q'),
                ..
            } => EventResult::ClosePanel,

            // Tab — 切换 Run（循环）
            Input { key: Key::Tab, .. } => {
                self.next_run();
                EventResult::Consumed
            }

            // ←/→ — 在 Phases（左）和 Agents（右）之间切换焦点
            Input { key: Key::Left, .. } => {
                self.focus = FocusZone::Phases;
                EventResult::Consumed
            }
            Input {
                key: Key::Right, ..
            } => {
                self.focus = FocusZone::Agents;
                EventResult::Consumed
            }

            // ↑/↓ — 当前列表内导航
            Input { key: Key::Up, .. } => {
                match self.focus {
                    FocusZone::Phases => self.move_phase_cursor(-1),
                    FocusZone::Agents => self.move_agent_cursor(-1),
                }
                EventResult::Consumed
            }
            Input { key: Key::Down, .. } => {
                match self.focus {
                    FocusZone::Phases => self.move_phase_cursor(1),
                    FocusZone::Agents => self.move_agent_cursor(1),
                }
                EventResult::Consumed
            }

            // x — kill 当前 agent（GAP-07）
            Input {
                key: Key::Char('x'),
                ..
            } => {
                if let Some(ref acp_client) = _ctx.acp_client {
                    if let Some((run_id, agent_id)) = self.selected_agent() {
                        let acp = acp_client.clone();
                        tokio::spawn(async move {
                            let params = serde_json::json!({
                                "runId": run_id,
                                "agentId": agent_id
                            });
                            let _ = acp.send_raw_request("workflow/kill_agent", params).await;
                        });
                    }
                }
                EventResult::Consumed
            }

            // d — kill 整个 workflow（GAP-07）
            Input {
                key: Key::Char('d'),
                ..
            } => {
                if let Some(ref acp_client) = _ctx.acp_client {
                    if let Some(run_id) = self.selected_run_id() {
                        let acp = acp_client.clone();
                        tokio::spawn(async move {
                            let params = serde_json::json!({ "runId": run_id });
                            let _ = acp.send_raw_request("workflow/kill_run", params).await;
                        });
                    }
                }
                EventResult::Consumed
            }

            // r — resume workflow（GAP-04）
            Input {
                key: Key::Char('r'),
                ..
            } => {
                if let Some(ref acp_client) = _ctx.acp_client {
                    if let Some(run_id) = self.selected_run_id() {
                        let acp = acp_client.clone();
                        tokio::spawn(async move {
                            let params = serde_json::json!({ "runId": run_id });
                            let _ = acp.send_raw_request("workflow/resume", params).await;
                        });
                    }
                }
                EventResult::Consumed
            }

            _ => EventResult::Consumed,
        }
    }

    fn desired_height(&self, _screen_height: u16, _screen_width: u16) -> u16 {
        20
    }

    fn render(&mut self, f: &mut Frame, _app: &mut App, area: Rect) {
        let block = Block::default().borders(Borders::ALL).title(Span::styled(
            " Workflows ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

        if self.runs.is_empty() {
            f.render_widget(
                ratatui::widgets::Paragraph::new("  No active workflows")
                    .style(Style::default().fg(Color::DarkGray)),
                block.inner(area),
            );
            f.render_widget(block, area);
            return;
        }

        // Layout: top = run tabs (3 lines), bottom = split tree
        let inner = block.inner(area);
        f.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(inner);

        let tabs_area = chunks[0];
        let tree_area = chunks[1];

        // ── Render run tabs ──
        self.render_tabs(f, tabs_area, &self.cached_tab_labels);

        // ── Render selected run's phases + agents ──
        // Clone phases (lightweight: usually 3-10 items) to avoid &mut self borrow conflict
        // with render_phases/render_agents below. Agents 从 run 直接构建一次 Vec，避免
        // 先 clone 全量 agents 再 clone 第二次（S-PERF1）。
        let run = match self.selected_run_data() {
            Some(r) => r,
            None => return,
        };
        let phases = run.phases.clone();
        let selected_title = phases.get(self.phase_cursor).map(|p| p.title.as_str());
        let filtered_agents: Vec<WorkflowAgentSnapshot> = if phases.is_empty() {
            run.agents.clone()
        } else {
            run.agents
                .iter()
                .filter(|a| a.phase.as_deref() == selected_title)
                .cloned()
                .collect()
        };

        let tree_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(tree_area);

        // Left: Phases
        self.render_phases(f, tree_chunks[0], &phases);

        // Right: Agents (filtered by selected phase)
        self.render_agents(f, tree_chunks[1], &filtered_agents, !phases.is_empty());
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn status_bar_hints(&self, _lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            ("Tab".into(), "Switch run".into()),
            ("←/→".into(), "Focus phase/agent".into()),
            ("↑/↓".into(), "Navigate".into()),
            ("x".into(), "Kill agent".into()),
            ("d".into(), "Kill workflow".into()),
            ("r".into(), "Resume".into()),
            ("Esc".into(), "Close".into()),
        ]
    }
}

impl WorkflowPanel {
    /// 渲染 Run tabs（简化版 TabBar）。
    fn render_tabs(&self, f: &mut Frame, area: Rect, labels: &[String]) {
        let spans: Vec<Span> = labels
            .iter()
            .enumerate()
            .flat_map(|(i, label)| {
                let is_selected = i == self.selected_run;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let separator = if i > 0 { " │ " } else { "" };
                vec![Span::raw(separator), Span::styled(label.as_str(), style)]
            })
            .collect();

        let line = Line::from(spans);
        let block = Block::default().borders(Borders::BOTTOM);
        f.render_widget(ratatui::widgets::Paragraph::new(line).block(block), area);
    }

    /// 渲染 Phase 列表（左栏）。
    fn render_phases(&mut self, f: &mut Frame, area: Rect, phases: &[WorkflowPhaseSnapshot]) {
        let is_focused = self.focus == FocusZone::Phases;
        let title_style = if is_focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Phases ", title_style));

        if phases.is_empty() {
            f.render_widget(
                ratatui::widgets::Paragraph::new("  (no phases)")
                    .style(Style::default().fg(Color::DarkGray)),
                block.inner(area),
            );
            f.render_widget(block, area);
            return;
        }

        let items: Vec<ListItem> = phases
            .iter()
            .map(|phase| {
                let icon = Self::status_icon(&phase.status);
                let color = Self::status_color(&phase.status);
                ListItem::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(format!("{icon} "), Style::default().fg(color)),
                    Span::raw(&phase.title),
                ]))
            })
            .collect();

        self.phase_state.select(Some(self.phase_cursor));
        let highlight = if is_focused {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::Indexed(236))
        };

        let list = List::new(items).block(block).highlight_style(highlight);
        f.render_stateful_widget(list, area, &mut self.phase_state);
    }

    /// 渲染 Agent 列表（右栏）。
    /// `phase_filter_active` = true 表示左侧 phases 非空，当前是按选中 phase 筛选后的子集；
    /// false 表示 run 没有 phases，agents 区展示全部。
    fn render_agents(
        &mut self,
        f: &mut Frame,
        area: Rect,
        agents: &[WorkflowAgentSnapshot],
        phase_filter_active: bool,
    ) {
        let is_focused = self.focus == FocusZone::Agents;
        let title_style = if is_focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Agents ", title_style));

        if agents.is_empty() {
            let msg = if phase_filter_active {
                "  (no agents in this phase)"
            } else {
                "  (no agents)"
            };
            f.render_widget(
                ratatui::widgets::Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
                block.inner(area),
            );
            f.render_widget(block, area);
            return;
        }

        let items: Vec<ListItem> = agents
            .iter()
            .map(|agent| {
                let icon = Self::status_icon(&agent.status);
                let color = Self::status_color(&agent.status);
                let label = agent.label.as_deref().unwrap_or("unnamed");
                let mut spans = vec![
                    Span::raw(" "),
                    Span::styled(format!("{icon} "), Style::default().fg(color)),
                    Span::styled(
                        format!("#{} {}", agent.agent_id, label),
                        Style::default().fg(color),
                    ),
                ];
                if let Some(tc) = agent.token_count {
                    spans.push(Span::styled(
                        format!("  {tc} tokens"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                if let Some(tool_count) = agent.tool_count {
                    spans.push(Span::styled(
                        format!(" {tool_count} tools"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                if let Some(ref phase) = agent.phase {
                    spans.push(Span::styled(
                        format!("  [{phase}]"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        self.agent_state.select(Some(self.agent_cursor));
        let highlight = if is_focused {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::Indexed(236))
        };

        let list = List::new(items).block(block).highlight_style(highlight);
        f.render_stateful_widget(list, area, &mut self.agent_state);
    }
}

#[cfg(test)]
#[path = "workflow_panel_test.rs"]
mod tests;
