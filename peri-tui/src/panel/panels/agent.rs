//! v2 AgentPanel -- Agent selection panel (PanelState trait implementation).
//!
//! Displays a list of available agents (scanned from `.claude/agents/`) plus a
//! "No Agent" sentinel entry. The user navigates with arrow keys, selects with
//! Enter, and closes with Esc.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use tui_textarea::Input;

use peri_widgets::{BorderedPanel, ScrollState, ScrollableArea};

use crate::app::panel_types::PanelKind;
use crate::command::panel::AgentItem;
use crate::panel::PanelState;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// Minimal scroll/cursor helper for the agent panel.
// ---------------------------------------------------------------------------

/// Lightweight list navigator for the agent panel.
#[derive(Debug, Clone)]
struct ListNav {
    /// Number of items (including the "No Agent" sentinel at index 0).
    len: usize,
    cursor: usize,
    scroll_offset: u16,
}

impl ListNav {
    fn new(len: usize) -> Self {
        let cursor = 0;
        Self {
            len,
            cursor,
            scroll_offset: 0,
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.len == 0 {
            return;
        }
        let max = self.len - 1;
        let new = self.cursor as isize + delta;
        self.cursor = (new.clamp(0, max as isize)) as usize;
    }

    fn ensure_visible(&mut self, visible_height: u16) {
        if visible_height == 0 || self.len == 0 {
            return;
        }
        let cursor = self.cursor as u16;
        if cursor < self.scroll_offset {
            self.scroll_offset = cursor;
        } else if cursor >= self.scroll_offset + visible_height {
            self.scroll_offset = cursor.saturating_sub(visible_height.saturating_sub(1));
        }
    }

    fn handle_scroll(&mut self, lines: i16, visible_height: u16) {
        if self.len == 0 || visible_height == 0 {
            return;
        }
        let max_scroll = (self.len as u16).saturating_sub(visible_height);
        let new_offset = self.scroll_offset as i16 + lines;
        self.scroll_offset = (new_offset.clamp(0, max_scroll as i16)) as u16;
    }

    fn scroll_offset(&self) -> u16 {
        self.scroll_offset
    }

    /// Handle a mouse click inside the panel. Returns true if an item was hit.
    fn handle_mouse_click(&mut self, mouse_row: u16, area: Rect, border_top: u16) -> bool {
        let relative_y = mouse_row.saturating_sub(area.y);
        if relative_y < border_top {
            return false;
        }
        let item_row = relative_y - border_top;
        let idx = item_row as usize + self.scroll_offset as usize;
        if idx < self.len {
            self.cursor = idx;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// AgentPanel
// ---------------------------------------------------------------------------

/// v2 Agent selection panel.
///
/// UI-local state only (cursor, scroll). Agent data is injected at
/// construction time. Side-effects (selecting an agent, closing the panel)
/// are returned as `PanelEffect` values.
#[derive(Debug)]
pub struct AgentPanel {
    /// Available agents (scanned from disk).
    agents: Vec<AgentItem>,
    /// Currently active agent_id (from session state, for highlighting).
    selected_id: Option<String>,
    /// List navigation state.
    nav: ListNav,
}

impl AgentPanel {
    /// Construct an empty panel for the registry factory.
    /// Data is populated on first render via `PanelReadContext`.
    pub fn empty() -> Self {
        Self::new(Vec::new(), None)
    }

    /// Construct a panel from live App data.
    ///
    /// Agent data arrives asynchronously via ACP queries (scan results
    /// from `.claude/agents/`). Currently delegates to `empty()` with
    /// data populated later via `set_agents()` when ACP results arrive.
    pub fn from_app(_app: &crate::app::App) -> Self {
        Self::empty()
    }

    /// Construct a new agent panel.
    ///
    /// `agents` is the list of available agents. `current_id` is the currently
    /// active agent_id (highlighted in the list).
    pub fn new(agents: Vec<AgentItem>, current_id: Option<String>) -> Self {
        let total = 1 + agents.len(); // +1 for "No Agent" sentinel
        // Position cursor on the currently active agent
        let cursor = current_id
            .as_ref()
            .and_then(|id| agents.iter().position(|a| &a.id == id))
            .map(|i| i + 1)
            .unwrap_or(0);

        let mut nav = ListNav::new(total);
        for _ in 0..cursor {
            nav.move_cursor(1);
        }

        Self {
            agents,
            selected_id: current_id,
            nav,
        }
    }

    /// Cursor position (0 = "No Agent", 1+ = agents index).
    fn cursor(&self) -> usize {
        self.nav.cursor
    }

    /// Get the selection at the current cursor.
    /// Returns (is_none, agent_id).
    fn get_selection(&self) -> (bool, Option<String>) {
        if self.cursor() == 0 {
            (true, None)
        } else if let Some(agent) = self.agents.get(self.cursor() - 1) {
            (false, Some(agent.id.clone()))
        } else {
            (true, None)
        }
    }
}

impl PanelState for AgentPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Agent
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext) {
        let lc = ctx.lc;
        let agent_count = self.agents.len();

        let title = if agent_count == 0 {
            lc.tr("agent-panel-title-none")
        } else {
            lc.tr("agent-panel-title")
        };

        let inner = BorderedPanel::new(Span::styled(
            title,
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        let mut lines: Vec<Line> = Vec::new();

        // Entry 0: "No Agent" sentinel
        let is_none_cursor = self.cursor() == 0;
        let is_none_selected = self.selected_id.is_none();
        lines.push(Line::from(vec![
            Span::styled(
                if is_none_cursor { "> " } else { "  " },
                Style::default().fg(theme::THINKING),
            ),
            Span::styled(
                lc.tr("agent-panel-none-label"),
                if is_none_selected {
                    Style::default()
                        .fg(theme::SAGE)
                        .add_modifier(Modifier::BOLD)
                } else if is_none_cursor {
                    Style::default()
                        .fg(theme::THINKING)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::MUTED)
                },
            ),
        ]));
        lines.push(Line::from("")); // spacer

        // Agent list
        for (i, agent) in self.agents.iter().enumerate() {
            let cursor_idx = i + 1; // offset for sentinel
            let is_cursor = self.cursor() == cursor_idx;
            let is_selected = self.selected_id.as_ref() == Some(&agent.id);

            let bullet = if is_selected { "*" } else { "o" };
            let cursor_char = if is_cursor { ">" } else { " " };

            let name_style = if is_selected {
                Style::default()
                    .fg(theme::SAGE)
                    .add_modifier(Modifier::BOLD)
            } else if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };

            lines.push(Line::from(vec![
                Span::styled(format!("{} {}", cursor_char, bullet), name_style),
                Span::styled(format!(" {}", agent.name), name_style),
            ]));

            // Description line (secondary info)
            if !agent.description.is_empty() {
                let desc_style = Style::default().fg(theme::MUTED);
                let desc: String = agent.description.chars().take(50).collect();
                let desc = if agent.description.chars().count() > 50 {
                    format!("{}...", desc)
                } else {
                    desc
                };
                lines.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(desc, desc_style),
                ]));
            } else {
                lines.push(Line::from(""));
            }
        }

        // Empty list hint
        if agent_count == 0 {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                lc.tr("agent-panel-empty-hint"),
                Style::default().fg(theme::MUTED),
            )));
        }

        // Render scrollable content
        let scroll_offset = self.nav.scroll_offset();
        let mut scroll_state = ScrollState::with_offset(scroll_offset);
        let _metrics = ScrollableArea::new(Text::from(lines))
            .scrollbar_style(Style::default().fg(theme::MUTED))
            .render(f, inner, &mut scroll_state);
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;
        match input {
            // Ctrl+C: not consumed, let parent handle
            Input {
                key: Key::Char('c'),
                ctrl: true,
                ..
            } => vec![],
            // Esc: close
            Input { key: Key::Esc, .. } => vec![PanelEffect::Close],
            // Up/Down: navigate
            Input { key: Key::Up, .. } => {
                self.nav.move_cursor(-1);
                self.nav.ensure_visible(10);
                vec![]
            }
            Input { key: Key::Down, .. } => {
                self.nav.move_cursor(1);
                self.nav.ensure_visible(10);
                vec![]
            }
            // Enter: confirm selection
            Input {
                key: Key::Enter, ..
            } => {
                let (is_none, agent_id) = self.get_selection();
                let agent_name = if is_none {
                    None
                } else {
                    agent_id.as_ref().and_then(|id| {
                        self.agents
                            .iter()
                            .find(|a| &a.id == id)
                            .map(|a| a.name.clone())
                    })
                };

                if is_none {
                    vec![
                        PanelEffect::ShowNotification(_ctx.lc.tr("app-agent-reset")),
                        PanelEffect::SendToAcp {
                            event: "set_agent_id".to_string(),
                            data: serde_json::json!({ "agent_id": null }),
                        },
                        PanelEffect::Close,
                    ]
                } else if let Some(id) = agent_id {
                    let name = agent_name.unwrap_or_else(|| id.clone());
                    vec![
                        PanelEffect::ShowNotification(_ctx.lc.tr_args(
                            "app-agent-switched",
                            &[
                                ("name".into(), name.into()),
                                ("id".into(), id.clone().into()),
                            ],
                        )),
                        PanelEffect::SendToAcp {
                            event: "set_agent_id".to_string(),
                            data: serde_json::json!({ "agent_id": id }),
                        },
                        PanelEffect::Close,
                    ]
                } else {
                    vec![PanelEffect::Close]
                }
            }
            // All other keys: consumed (no-op)
            _ => vec![],
        }
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        _ctx: &PanelReadContext,
    ) -> Vec<PanelEffect> {
        if mouse.kind == MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left)
            && self.nav.handle_mouse_click(mouse.row, area, 1)
        {
            // Simulate Enter on click
            return self.handle_key(
                Input::from(KeyEvent::new(
                    KeyCode::Enter,
                    ratatui::crossterm::event::KeyModifiers::NONE,
                )),
                _ctx,
            );
        }
        vec![]
    }

    fn handle_scroll(&mut self, lines: i16, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        self.nav.handle_scroll(lines, 10);
        vec![]
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        (self.agents.len() as u16 * 2 + 6).max(6)
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            ("Up/Down".to_string(), lc.tr("key-select")),
            ("Enter".to_string(), lc.tr("key-confirm")),
            ("Esc".to_string(), lc.tr("key-cancel")),
        ]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tui_textarea::Key;

    use super::*;
    use crate::command::panel::AgentItem;
    use crate::panel::PanelState;
    use crate::panel::read_context::{PanelReadContext, ServiceRegistrySnapshot};

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
            // SAFETY: the thread-local lives for the duration of the test and
            // we only access ctx within the same closure scope.
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
                            services: snapshot.clone(),
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

    fn make_agent(id: &str, name: &str, desc: &str) -> AgentItem {
        AgentItem {
            id: id.to_string(),
            name: name.to_string(),
            description: desc.to_string(),
        }
    }

    #[test]
    fn test_kind_returns_correct_variant() {
        let panel = AgentPanel::new(vec![], None);
        assert_eq!(panel.kind(), PanelKind::Agent);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = AgentPanel::new(vec![], None);
        let ctx = make_ctx();
        let effects = panel.handle_key(
            Input {
                key: Key::Esc,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_enter_selects_none_agent() {
        let mut panel = AgentPanel::new(vec![], None);
        let ctx = make_ctx();
        // Cursor starts at 0 ("No Agent")
        let effects = panel.handle_key(
            Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert!(effects.contains(&PanelEffect::Close));
        // Should contain a SendToAcp with agent_id: null
        let has_set_agent = effects.iter().any(|e| {
            matches!(
                e,
                PanelEffect::SendToAcp { event, data } if event == "set_agent_id" && data["agent_id"].is_null()
            )
        });
        assert!(
            has_set_agent,
            "Enter on 'No Agent' should emit set_agent_id(null)"
        );
    }

    #[test]
    fn test_enter_selects_specific_agent() {
        let agents = vec![
            make_agent("code-review", "Code Reviewer", "Reviews code changes"),
            make_agent("test-gen", "Test Generator", "Generates tests"),
        ];
        let mut panel = AgentPanel::new(agents, None);
        let ctx = make_ctx();

        // Move cursor down to first agent (index 1)
        panel.handle_key(
            Input {
                key: Key::Down,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 1);

        // Press Enter
        let effects = panel.handle_key(
            Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert!(effects.contains(&PanelEffect::Close));
        let has_set_agent = effects.iter().any(|e| {
            matches!(
                e,
                PanelEffect::SendToAcp { event, data } if event == "set_agent_id" && data["agent_id"] == "code-review"
            )
        });
        assert!(
            has_set_agent,
            "Enter on agent should emit set_agent_id with correct id"
        );
    }

    #[test]
    fn test_up_down_navigation() {
        let agents = vec![
            make_agent("a", "A", ""),
            make_agent("b", "B", ""),
            make_agent("c", "C", ""),
        ];
        let mut panel = AgentPanel::new(agents, None);
        let ctx = make_ctx();

        // Starts at 0 (No Agent)
        assert_eq!(panel.cursor(), 0);

        // Down -> 1
        panel.handle_key(
            Input {
                key: Key::Down,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 1);

        // Down -> 2
        panel.handle_key(
            Input {
                key: Key::Down,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 2);

        // Down -> 3
        panel.handle_key(
            Input {
                key: Key::Down,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 3);

        // Down -> clamped at 3
        panel.handle_key(
            Input {
                key: Key::Down,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 3);

        // Up -> 2
        panel.handle_key(
            Input {
                key: Key::Up,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 2);

        // Up -> 1
        panel.handle_key(
            Input {
                key: Key::Up,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 1);

        // Up -> 0
        panel.handle_key(
            Input {
                key: Key::Up,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 0);

        // Up -> clamped at 0
        panel.handle_key(
            Input {
                key: Key::Up,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 0);
    }

    #[test]
    fn test_cursor_initial_position_matches_selected_id() {
        let agents = vec![
            make_agent("alpha", "Alpha", ""),
            make_agent("beta", "Beta", ""),
            make_agent("gamma", "Gamma", ""),
        ];
        // selected_id = "gamma" -> cursor should be at 3 (index 2 + 1)
        let panel = AgentPanel::new(agents, Some("gamma".to_string()));
        assert_eq!(panel.cursor(), 3);
    }

    #[test]
    fn test_desired_height() {
        let agents = vec![make_agent("a", "A", ""), make_agent("b", "B", "")];
        let panel = AgentPanel::new(agents, None);
        // 2 agents * 2 + 6 = 10
        assert_eq!(panel.desired_height(50, 80), 10);
    }

    #[test]
    fn test_render_does_not_panic() {
        let agents = vec![
            make_agent("code-review", "Code Reviewer", "Reviews code"),
            make_agent("test-gen", "Test Generator", "Generates unit tests"),
        ];
        let mut panel = AgentPanel::new(agents, Some("code-review".to_string()));
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_empty_agents_does_not_panic() {
        let mut panel = AgentPanel::new(vec![], None);
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = AgentPanel::new(vec![], None);
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        // Should have 3 hints: select, confirm, cancel
        assert_eq!(hints.len(), 3);
    }

    #[test]
    fn test_ctrl_c_not_consumed() {
        let mut panel = AgentPanel::new(vec![], None);
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
        // Ctrl+C should produce no effects (let parent handle it)
        assert!(effects.is_empty());
    }
}
