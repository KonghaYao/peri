//! v2 WorkflowPanel -- Workflow execution progress panel (PanelState trait implementation).
//!
//! Three-level tree display: Run tabs (top) -> Phase list (left) -> Agent list (right).
//! Shows running/completed/failed workflow runs with their phases and agents.
//!
//! Navigation: Tab to switch runs; Left/Right to toggle focus between Phases and Agents;
//! Up/Down to navigate within the focused list.
//! Actions: x = kill agent, d = kill workflow, r = resume workflow.
//! Close: Esc or q.
//!
//! TODO (P3 Integration): `PanelReadContext` currently has no workflow data fields.
//! The panel defines internal DTO types (`WorkflowRunEntry`, `WorkflowPhaseEntry`,
//! `WorkflowAgentEntry`) for the UI state. During P3 Integration, the state machine
//! will populate panel data via a `set_runs()` method after ACP query results arrive,
//! or `ServiceRegistrySnapshot` will gain a `workflows` field.

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tui_textarea::Input;

use crate::app::panel_manager::PanelKind;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::panel::PanelState;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// Internal DTO types (panel-local, no runtime dependency)
// ---------------------------------------------------------------------------

/// A workflow run entry for display.
#[derive(Debug, Clone)]
pub struct WorkflowRunEntry {
    pub run_id: String,
    pub workflow_name: String,
    pub status: String,
    pub phases: Vec<WorkflowPhaseEntry>,
    pub agents: Vec<WorkflowAgentEntry>,
}

/// A phase within a workflow run.
#[derive(Debug, Clone)]
pub struct WorkflowPhaseEntry {
    pub title: String,
    pub status: String,
}

/// An agent within a workflow run (optionally associated with a phase).
#[derive(Debug, Clone)]
pub struct WorkflowAgentEntry {
    pub agent_id: u64,
    pub label: Option<String>,
    pub phase: Option<String>,
    pub status: String,
    pub token_count: Option<u64>,
    pub tool_count: Option<u64>,
}

// ---------------------------------------------------------------------------
// FocusZone
// ---------------------------------------------------------------------------

/// Which side panel is focused (Phases = left, Agents = right).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusZone {
    Phases,
    Agents,
}

// ---------------------------------------------------------------------------
// WorkflowPanel
// ---------------------------------------------------------------------------

/// v2 Workflow execution progress panel.
///
/// Three-level tree layout: Run tabs (top) -> Phase list (left) -> Agent list (right).
/// Actions (kill agent/workflow, resume) are returned as `PanelEffect::SendToAcp`
/// instructions; the state machine translates them to actual ACP operations.
#[derive(Debug)]
pub struct WorkflowPanel {
    /// Workflow run data.
    runs: Vec<WorkflowRunEntry>,
    /// Currently selected Run tab index.
    selected_run: usize,
    /// Phase list cursor.
    phase_cursor: usize,
    /// Phase list state (for ratatui List rendering).
    phase_state: ListState,
    /// Agent list cursor.
    agent_cursor: usize,
    /// Agent list state (for ratatui List rendering).
    agent_state: ListState,
    /// Current focus zone.
    focus: FocusZone,
    /// Pre-cached tab labels (avoid format! in render hot path).
    cached_tab_labels: Vec<String>,
}

impl WorkflowPanel {
    /// Create an empty panel (no runs loaded yet).
    ///
    /// Used by the registry factory. Runs can be populated later via
    /// `set_runs()` when ACP query results arrive.
    pub fn empty() -> Self {
        Self {
            runs: Vec::new(),
            selected_run: 0,
            phase_cursor: 0,
            phase_state: ListState::default(),
            agent_cursor: 0,
            agent_state: ListState::default(),
            focus: FocusZone::Agents,
            cached_tab_labels: Vec::new(),
        }
    }

    /// Create a panel from a list of run entries.
    pub fn new(runs: Vec<WorkflowRunEntry>) -> Self {
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

    /// Replace run data (e.g. after ACP query results arrive).
    pub fn set_runs(&mut self, runs: Vec<WorkflowRunEntry>) {
        self.cached_tab_labels = Self::build_tab_labels(&runs);
        self.runs = runs;
        if self.selected_run >= self.runs.len() && !self.runs.is_empty() {
            self.selected_run = self.runs.len().saturating_sub(1);
        }
        self.clamp_cursors();
    }

    /// Build tab label strings (precomputed to avoid format! per frame).
    fn build_tab_labels(runs: &[WorkflowRunEntry]) -> Vec<String> {
        runs.iter()
            .map(|r| {
                let icon = Self::status_icon(&r.status);
                let short_id = &r.run_id[..8.min(r.run_id.len())];
                format!(" {icon} {} [{short_id}] ", r.workflow_name)
            })
            .collect()
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

    fn selected_run_data(&self) -> Option<&WorkflowRunEntry> {
        self.runs.get(self.selected_run)
    }

    /// Number of agents filtered by the currently selected phase.
    /// When run has no phases, returns the total agent count.
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
        // Phase switch resets agent cursor (filter changes)
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

    /// Currently selected agent (for kill action).
    fn selected_agent(&self) -> Option<(String, u64)> {
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

    /// Currently selected run_id (for kill workflow action).
    fn selected_run_id(&self) -> Option<String> {
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
            "running" | "active" => "\u{25b6}", // >
            "completed" | "done" => "\u{2713}", // check
            "failed" | "dead" => "\u{2717}",    // cross
            "killed" => "\u{2620}",             // skull
            "skipped" => "\u{2298}",            // circled slash
            "pending" => "\u{25cb}",            // circle
            _ => "\u{2022}",                    // bullet
        }
    }
}

impl PanelState for WorkflowPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Workflow
    }

    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelReadContext) {
        let block = Block::default().borders(Borders::ALL).title(Span::styled(
            " Workflows ",
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ));

        if self.runs.is_empty() {
            f.render_widget(
                Paragraph::new("  No active workflows").style(Style::default().fg(theme::MUTED)),
                block.inner(area),
            );
            f.render_widget(block, area);
            return;
        }

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Layout: top = run tabs (3 lines), bottom = split tree
        let chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Length(3),
                ratatui::layout::Constraint::Min(1),
            ])
            .split(inner);

        // Render run tabs
        self.render_tabs(f, chunks[0]);

        // Collect phase/agent data for selected run (clone to avoid &mut self conflict)
        let run = match self.selected_run_data() {
            Some(r) => r,
            None => return,
        };
        let phases = run.phases.clone();
        let selected_title = phases.get(self.phase_cursor).map(|p| p.title.as_str());
        let filtered_agents: Vec<WorkflowAgentEntry> = if phases.is_empty() {
            run.agents.clone()
        } else {
            run.agents
                .iter()
                .filter(|a| a.phase.as_deref() == selected_title)
                .cloned()
                .collect()
        };

        let tree_chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([
                ratatui::layout::Constraint::Percentage(30),
                ratatui::layout::Constraint::Percentage(70),
            ])
            .split(chunks[1]);

        // Left: Phases
        self.render_phases(f, tree_chunks[0], &phases);

        // Right: Agents (filtered by selected phase)
        self.render_agents(f, tree_chunks[1], &filtered_agents, !phases.is_empty());
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;

        match input {
            // Esc / q: close panel
            Input { key: Key::Esc, .. }
            | Input {
                key: Key::Char('q'),
                ..
            } => {
                vec![PanelEffect::Close]
            }

            // Tab: cycle through runs
            Input { key: Key::Tab, .. } => {
                self.next_run();
                vec![]
            }

            // Left/Right: toggle focus between Phases and Agents
            Input { key: Key::Left, .. } => {
                self.focus = FocusZone::Phases;
                vec![]
            }
            Input {
                key: Key::Right, ..
            } => {
                self.focus = FocusZone::Agents;
                vec![]
            }

            // Up/Down: navigate within focused list
            Input { key: Key::Up, .. } => {
                match self.focus {
                    FocusZone::Phases => self.move_phase_cursor(-1),
                    FocusZone::Agents => self.move_agent_cursor(-1),
                }
                vec![]
            }
            Input { key: Key::Down, .. } => {
                match self.focus {
                    FocusZone::Phases => self.move_phase_cursor(1),
                    FocusZone::Agents => self.move_agent_cursor(1),
                }
                vec![]
            }

            // x: kill current agent
            Input {
                key: Key::Char('x'),
                ..
            } => {
                if let Some((run_id, agent_id)) = self.selected_agent() {
                    return vec![PanelEffect::SendToAcp {
                        event: "workflow/kill_agent".to_string(),
                        data: serde_json::json!({
                            "runId": run_id,
                            "agentId": agent_id
                        }),
                    }];
                }
                vec![]
            }

            // d: kill entire workflow
            Input {
                key: Key::Char('d'),
                ..
            } => {
                if let Some(run_id) = self.selected_run_id() {
                    return vec![PanelEffect::SendToAcp {
                        event: "workflow/kill_run".to_string(),
                        data: serde_json::json!({ "runId": run_id }),
                    }];
                }
                vec![]
            }

            // r: resume workflow
            Input {
                key: Key::Char('r'),
                ..
            } => {
                if let Some(run_id) = self.selected_run_id() {
                    return vec![PanelEffect::SendToAcp {
                        event: "workflow/resume".to_string(),
                        data: serde_json::json!({ "runId": run_id }),
                    }];
                }
                vec![]
            }

            // Ctrl+C: not consumed, let upper layer handle
            Input {
                key: Key::Char('c'),
                ctrl: true,
                ..
            } => vec![],

            // All other keys: consumed, no-op
            _ => vec![],
        }
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        _ctx: &PanelReadContext,
    ) -> Vec<PanelEffect> {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            let relative_y = mouse.row.saturating_sub(area.y);
            let relative_x = mouse.column.saturating_sub(area.x);
            if relative_y <= 3 {
                // Click on tab bar: find nearest tab
                // Simplified: just cycle to next run
                self.next_run();
            } else if relative_x < area.width / 3 {
                // Click on left half: focus phases
                self.focus = FocusZone::Phases;
                // Approximate cursor from click position
                let header = 3; // border + tab area
                let clicked = relative_y.saturating_sub(header) as usize;
                let phase_count = self
                    .selected_run_data()
                    .map(|r| r.phases.len())
                    .unwrap_or(0);
                if clicked < phase_count {
                    self.phase_cursor = clicked;
                }
            } else {
                // Click on right half: focus agents
                self.focus = FocusZone::Agents;
            }
        }
        vec![]
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        20
    }

    fn status_bar_hints(&self, _lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            ("Tab".to_string(), "Switch run".to_string()),
            (
                "\u{2190}/\u{2192}".to_string(),
                "Focus phase/agent".to_string(),
            ),
            ("\u{2191}/\u{2193}".to_string(), "Navigate".to_string()),
            ("x".to_string(), "Kill agent".to_string()),
            ("d".to_string(), "Kill workflow".to_string()),
            ("r".to_string(), "Resume".to_string()),
            ("Esc".to_string(), "Close".to_string()),
        ]
    }
}

// ---------------------------------------------------------------------------
// Private rendering helpers
// ---------------------------------------------------------------------------

impl WorkflowPanel {
    /// Render run tabs (simplified TabBar).
    fn render_tabs(&self, f: &mut Frame, area: Rect) {
        let spans: Vec<Span> = self
            .cached_tab_labels
            .iter()
            .enumerate()
            .flat_map(|(i, label)| {
                let is_selected = i == self.selected_run;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(theme::THINKING)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::MUTED)
                };
                let separator = if i > 0 { " \u{2502} " } else { "" };
                vec![Span::raw(separator), Span::styled(label.as_str(), style)]
            })
            .collect();

        let line = Line::from(spans);
        let block = Block::default().borders(Borders::BOTTOM);
        f.render_widget(Paragraph::new(line).block(block), area);
    }

    /// Render phase list (left column).
    fn render_phases(&mut self, f: &mut Frame, area: Rect, phases: &[WorkflowPhaseEntry]) {
        let is_focused = self.focus == FocusZone::Phases;
        let title_style = if is_focused {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::DIM)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Phases ", title_style));

        if phases.is_empty() {
            f.render_widget(
                Paragraph::new("  (no phases)").style(Style::default().fg(theme::MUTED)),
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

    /// Render agent list (right column).
    /// `phase_filter_active` = true means phases exist and agents are filtered.
    fn render_agents(
        &mut self,
        f: &mut Frame,
        area: Rect,
        agents: &[WorkflowAgentEntry],
        phase_filter_active: bool,
    ) {
        let is_focused = self.focus == FocusZone::Agents;
        let title_style = if is_focused {
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::DIM)
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
                Paragraph::new(msg).style(Style::default().fg(theme::MUTED)),
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
                        Style::default().fg(theme::MUTED),
                    ));
                }
                if let Some(tool_count) = agent.tool_count {
                    spans.push(Span::styled(
                        format!(" {tool_count} tools"),
                        Style::default().fg(theme::MUTED),
                    ));
                }
                if let Some(ref phase) = agent.phase {
                    spans.push(Span::styled(
                        format!("  [{phase}]"),
                        Style::default().fg(theme::MUTED),
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tui_textarea::Key;

    use super::*;
    use crate::panel::read_context::{PanelReadContext, ServiceRegistrySnapshot};
    use crate::panel::PanelState;

    /// Helper: build a minimal `PanelReadContext` for testing.
    fn make_ctx() -> PanelReadContext<'static> {
        thread_local! {
            static SNAPSHOT: ServiceRegistrySnapshot = ServiceRegistrySnapshot::new();
            static VMS: Vec<peri_acp_types::view_model::ViewModel> = const { Vec::new() };
            #[allow(clippy::missing_const_for_thread_local)]
            static CACHE: HashMap<String, serde_json::Value> = HashMap::new();
            static LC: crate::i18n::LcRegistry = crate::i18n::LcRegistry::default();
        }
        SNAPSHOT.with(|snapshot| {
            let snapshot: &'static ServiceRegistrySnapshot = unsafe { &*(snapshot as *const _) };
            VMS.with(|vms| {
                let vms: &'static Vec<peri_acp_types::view_model::ViewModel> =
                    unsafe { &*(vms as *const _) };
                CACHE.with(|cache| {
                    let cache: &'static HashMap<String, serde_json::Value> =
                        unsafe { &*(cache as *const _) };
                    LC.with(|lc| {
                        let lc: &'static crate::i18n::LcRegistry = unsafe { &*(lc as *const _) };
                        PanelReadContext {
                            services: snapshot,
                            view_models: vms,
                            scroll_offset: 0,
                            area: Rect::new(0, 0, 80, 24),
                            lc,
                            acp_query_cache: cache,
                        }
                    })
                })
            })
        })
    }

    fn esc_input() -> Input {
        Input {
            key: Key::Esc,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn up_input() -> Input {
        Input {
            key: Key::Up,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn down_input() -> Input {
        Input {
            key: Key::Down,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn left_input() -> Input {
        Input {
            key: Key::Left,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn right_input() -> Input {
        Input {
            key: Key::Right,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn tab_input() -> Input {
        Input {
            key: Key::Tab,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    fn char_input(c: char) -> Input {
        Input {
            key: Key::Char(c),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// 构造测试用 WorkflowRunEntry。
    fn make_run(
        id: &str,
        name: &str,
        status: &str,
        phases: Vec<WorkflowPhaseEntry>,
        agents: Vec<WorkflowAgentEntry>,
    ) -> WorkflowRunEntry {
        WorkflowRunEntry {
            run_id: id.to_string(),
            workflow_name: name.to_string(),
            status: status.to_string(),
            phases,
            agents,
        }
    }

    fn make_phase(title: &str, status: &str) -> WorkflowPhaseEntry {
        WorkflowPhaseEntry {
            title: title.to_string(),
            status: status.to_string(),
        }
    }

    fn make_agent(id: u64, label: &str, phase: Option<&str>, status: &str) -> WorkflowAgentEntry {
        WorkflowAgentEntry {
            agent_id: id,
            label: Some(label.to_string()),
            phase: phase.map(|s| s.to_string()),
            status: status.to_string(),
            token_count: None,
            tool_count: None,
        }
    }

    #[test]
    fn test_kind_returns_correct_variant() {
        let panel = WorkflowPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Workflow);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = WorkflowPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(esc_input(), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_q_also_closes() {
        let mut panel = WorkflowPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(char_input('q'), &ctx);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_render_does_not_panic_empty() {
        let mut panel = WorkflowPanel::empty();
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_with_runs() {
        let runs = vec![make_run(
            "run-00000001-abcdef",
            "deploy-pipeline",
            "running",
            vec![
                make_phase("build", "completed"),
                make_phase("test", "running"),
                make_phase("deploy", "pending"),
            ],
            vec![
                make_agent(1, "builder", Some("build"), "completed"),
                make_agent(2, "tester", Some("test"), "running"),
                make_agent(3, "deployer", Some("deploy"), "pending"),
            ],
        )];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_does_not_panic_multiple_runs() {
        let runs = vec![
            make_run(
                "run-00000001-abcdef",
                "deploy-pipeline",
                "running",
                vec![make_phase("build", "completed")],
                vec![make_agent(1, "builder", Some("build"), "completed")],
            ),
            make_run(
                "run-00000002-ghijkl",
                "test-suite",
                "completed",
                vec![
                    make_phase("unit", "completed"),
                    make_phase("integration", "completed"),
                ],
                vec![
                    make_agent(10, "unit-runner", Some("unit"), "completed"),
                    make_agent(11, "int-runner", Some("integration"), "completed"),
                ],
            ),
        ];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_tab_switches_run() {
        let runs = vec![
            make_run(
                "run-00000001-abcdef",
                "deploy-pipeline",
                "running",
                vec![],
                vec![],
            ),
            make_run(
                "run-00000002-ghijkl",
                "test-suite",
                "completed",
                vec![],
                vec![],
            ),
        ];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();

        assert_eq!(panel.selected_run, 0);
        panel.handle_key(tab_input(), &ctx);
        assert_eq!(panel.selected_run, 1);
        panel.handle_key(tab_input(), &ctx);
        assert_eq!(panel.selected_run, 0); // wraps around
    }

    #[test]
    fn test_left_right_switches_focus() {
        let mut panel = WorkflowPanel::empty();
        let ctx = make_ctx();

        // Default focus is Agents
        assert_eq!(panel.focus, FocusZone::Agents);

        panel.handle_key(left_input(), &ctx);
        assert_eq!(panel.focus, FocusZone::Phases);

        panel.handle_key(right_input(), &ctx);
        assert_eq!(panel.focus, FocusZone::Agents);
    }

    #[test]
    fn test_arrow_keys_move_phase_cursor() {
        let runs = vec![make_run(
            "run-00000001-abcdef",
            "deploy",
            "running",
            vec![
                make_phase("build", "completed"),
                make_phase("test", "running"),
                make_phase("deploy", "pending"),
            ],
            vec![],
        )];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();

        // Focus on phases
        panel.focus = FocusZone::Phases;

        // Initial cursor=0
        assert_eq!(panel.phase_cursor, 0);

        // Down -> cursor=1
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.phase_cursor, 1);

        // Down -> cursor=2
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.phase_cursor, 2);

        // Down -> clamped at 2
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.phase_cursor, 2);

        // Up -> cursor=1
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.phase_cursor, 1);

        // Up -> cursor=0
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.phase_cursor, 0);

        // Up -> clamped at 0
        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.phase_cursor, 0);
    }

    #[test]
    fn test_arrow_keys_move_agent_cursor() {
        let runs = vec![make_run(
            "run-00000001-abcdef",
            "deploy",
            "running",
            vec![],
            vec![
                make_agent(1, "a1", None, "running"),
                make_agent(2, "a2", None, "running"),
                make_agent(3, "a3", None, "pending"),
            ],
        )];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();

        // Default focus is Agents
        assert_eq!(panel.agent_cursor, 0);

        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.agent_cursor, 1);

        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.agent_cursor, 2);

        // Clamped
        panel.handle_key(down_input(), &ctx);
        assert_eq!(panel.agent_cursor, 2);

        panel.handle_key(up_input(), &ctx);
        assert_eq!(panel.agent_cursor, 1);
    }

    #[test]
    fn test_phase_switch_resets_agent_cursor() {
        let runs = vec![make_run(
            "run-00000001-abcdef",
            "deploy",
            "running",
            vec![
                make_phase("build", "completed"),
                make_phase("test", "running"),
            ],
            vec![
                make_agent(1, "builder", Some("build"), "completed"),
                make_agent(2, "tester", Some("build"), "completed"),
                make_agent(3, "tester-1", Some("test"), "running"),
                make_agent(4, "tester-2", Some("test"), "running"),
            ],
        )];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();

        // Focus on phases, initial phase_cursor=0
        panel.focus = FocusZone::Phases;
        // build phase has 2 agents, move agent cursor to 1
        panel.agent_cursor = 1;
        assert_eq!(panel.agent_cursor, 1);

        // Switch phase -> agent cursor resets
        panel.move_phase_cursor(1);
        assert_eq!(panel.phase_cursor, 1);
        assert_eq!(panel.agent_cursor, 0);
    }

    #[test]
    fn test_kill_agent_produces_send_to_acp() {
        let runs = vec![make_run(
            "run-00000001-abcdef",
            "deploy",
            "running",
            vec![],
            vec![make_agent(42, "worker", None, "running")],
        )];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();

        let effects = panel.handle_key(char_input('x'), &ctx);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            PanelEffect::SendToAcp { event, data } => {
                assert_eq!(event, "workflow/kill_agent");
                assert_eq!(data["runId"], "run-00000001-abcdef");
                assert_eq!(data["agentId"], 42);
            }
            _ => panic!("expected SendToAcp, got {:?}", effects[0]),
        }
    }

    #[test]
    fn test_kill_workflow_produces_send_to_acp() {
        let runs = vec![make_run(
            "run-00000001-abcdef",
            "deploy",
            "running",
            vec![],
            vec![],
        )];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();

        let effects = panel.handle_key(char_input('d'), &ctx);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            PanelEffect::SendToAcp { event, data } => {
                assert_eq!(event, "workflow/kill_run");
                assert_eq!(data["runId"], "run-00000001-abcdef");
            }
            _ => panic!("expected SendToAcp, got {:?}", effects[0]),
        }
    }

    #[test]
    fn test_resume_produces_send_to_acp() {
        let runs = vec![make_run(
            "run-00000001-abcdef",
            "deploy",
            "failed",
            vec![],
            vec![],
        )];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();

        let effects = panel.handle_key(char_input('r'), &ctx);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            PanelEffect::SendToAcp { event, data } => {
                assert_eq!(event, "workflow/resume");
                assert_eq!(data["runId"], "run-00000001-abcdef");
            }
            _ => panic!("expected SendToAcp, got {:?}", effects[0]),
        }
    }

    #[test]
    fn test_empty_panel_no_side_effects_for_actions() {
        let mut panel = WorkflowPanel::empty();
        let ctx = make_ctx();

        // No runs, so x/d/r produce no effects
        assert!(panel.handle_key(char_input('x'), &ctx).is_empty());
        assert!(panel.handle_key(char_input('d'), &ctx).is_empty());
        assert!(panel.handle_key(char_input('r'), &ctx).is_empty());
    }

    #[test]
    fn test_ctrl_c_not_consumed() {
        let mut panel = WorkflowPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(
            Input {
                key: Key::Char('c'),
                ctrl: true,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_other_keys_consumed_no_op() {
        let runs = vec![make_run(
            "run-00000001-abcdef",
            "deploy",
            "running",
            vec![],
            vec![],
        )];
        let mut panel = WorkflowPanel::new(runs);
        let ctx = make_ctx();
        let effects = panel.handle_key(char_input('a'), &ctx);
        assert_eq!(effects.len(), 0);
    }

    #[test]
    fn test_desired_height() {
        let panel = WorkflowPanel::empty();
        assert_eq!(panel.desired_height(50, 80), 20);
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = WorkflowPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 7);
    }

    #[test]
    fn test_set_runs_replaces_data() {
        let mut panel = WorkflowPanel::empty();
        assert!(panel.runs.is_empty());

        let runs = vec![make_run(
            "run-00000001-abcdef",
            "deploy",
            "running",
            vec![make_phase("build", "running")],
            vec![make_agent(1, "builder", Some("build"), "running")],
        )];
        panel.set_runs(runs);
        assert_eq!(panel.runs.len(), 1);
        assert_eq!(panel.selected_run, 0);
        assert_eq!(panel.cached_tab_labels.len(), 1);
    }

    #[test]
    fn test_set_runs_clamps_selected_run() {
        let runs = vec![
            make_run("run-01", "a", "running", vec![], vec![]),
            make_run("run-02", "b", "running", vec![], vec![]),
            make_run("run-03", "c", "running", vec![], vec![]),
        ];
        let mut panel = WorkflowPanel::new(runs);
        assert_eq!(panel.selected_run, 0);

        // Select last run
        panel.selected_run = 2;

        // Replace with fewer runs -> selected_run should be clamped
        let new_runs = vec![make_run("run-04", "x", "running", vec![], vec![])];
        panel.set_runs(new_runs);
        assert_eq!(panel.selected_run, 0);
    }

    #[test]
    fn test_status_color_and_icon() {
        // Verify known statuses produce non-default colors
        assert_eq!(WorkflowPanel::status_color("running"), Color::Yellow);
        assert_eq!(WorkflowPanel::status_color("completed"), Color::Green);
        assert_eq!(WorkflowPanel::status_color("failed"), Color::Red);
        assert_eq!(WorkflowPanel::status_color("pending"), Color::Gray);

        // Verify icons are non-empty for known statuses
        assert!(!WorkflowPanel::status_icon("running").is_empty());
        assert!(!WorkflowPanel::status_icon("completed").is_empty());
        assert!(!WorkflowPanel::status_icon("failed").is_empty());
        assert!(!WorkflowPanel::status_icon("unknown_status").is_empty());
    }
}
