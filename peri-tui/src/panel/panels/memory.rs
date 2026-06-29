//! v2 MemoryPanel -- Memory file list panel (PanelState trait implementation).
//!
//! Displays a list of CLAUDE.md memory files (project-level and global). The user
//! navigates with arrow keys, opens with Enter (delegates to editor -- TODO), and
//! closes with Esc.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::Frame;
use tui_textarea::Input;

use peri_widgets::{BorderedPanel, ScrollState, ScrollableArea};

use crate::app::panel_types::PanelKind;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::panel::PanelState;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// MemoryEntry
// ---------------------------------------------------------------------------

/// A single memory file entry displayed in the panel.
#[derive(Debug, Clone)]
struct MemoryEntry {
    label: String,
    path: PathBuf,
    exists: bool,
}

// ---------------------------------------------------------------------------
// ListNav (same lightweight helper as AgentPanel)
// ---------------------------------------------------------------------------

/// Lightweight list navigator for the memory panel.
#[derive(Debug, Clone)]
struct ListNav {
    len: usize,
    cursor: usize,
    scroll_offset: u16,
}

impl ListNav {
    fn new(len: usize) -> Self {
        Self {
            len,
            cursor: 0,
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
// MemoryPanel
// ---------------------------------------------------------------------------

/// v2 Memory file list panel.
///
/// UI-local state only (cursor, scroll, entries). Side-effects (open editor,
/// close panel) are returned as `PanelEffect` values.
#[derive(Debug)]
pub struct MemoryPanel {
    /// Memory file entries.
    entries: Vec<MemoryEntry>,
    /// List navigation state.
    nav: ListNav,
}

impl MemoryPanel {
    /// Construct an empty panel for the registry factory.
    pub fn empty() -> Self {
        Self::new(String::new(), None)
    }

    /// Construct a new memory panel.
    ///
    /// `cwd` is the project working directory (used to locate project-level
    /// `CLAUDE.md`). `home_dir` is used to locate the global `~/.claude/CLAUDE.md`.
    pub fn new(cwd: String, home_dir: Option<PathBuf>) -> Self {
        let project_path = PathBuf::from(&cwd).join("CLAUDE.md");
        let global_path = home_dir
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(".claude")
            .join("CLAUDE.md");

        let entries = vec![
            MemoryEntry {
                label: "Project".to_string(),
                path: project_path,
                exists: false,
            },
            MemoryEntry {
                label: "User".to_string(),
                path: global_path,
                exists: false,
            },
        ];

        let nav = ListNav::new(entries.len());

        Self { entries, nav }
    }

    /// Refresh all entries' `exists` status by checking the filesystem.
    fn refresh_exists(&mut self) {
        for entry in &mut self.entries {
            entry.exists = entry.path.exists();
        }
    }

    /// Cursor position (0-based index into entries).
    fn cursor(&self) -> usize {
        self.nav.cursor
    }
}

impl PanelState for MemoryPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Memory
    }

    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelReadContext) {
        // Refresh exists status on each render
        self.refresh_exists();

        let title = " Memory ";
        let inner = BorderedPanel::new(Span::styled(
            title,
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        let mut lines: Vec<Line> = Vec::new();

        for (i, entry) in self.entries.iter().enumerate() {
            let is_cursor = i == self.cursor();
            let cursor_char = if is_cursor { "> " } else { "  " };

            let style = if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };

            let (exist_icon, exist_style) = if entry.exists {
                ("Y", Style::default().fg(theme::SAGE))
            } else {
                ("N", Style::default().fg(theme::MUTED))
            };

            let path_str = entry.path.to_string_lossy();
            let path_display: String = if path_str.len() > 40 {
                format!("...{}", &path_str[path_str.len() - 37..])
            } else {
                path_str.to_string()
            };

            lines.push(Line::from(vec![
                Span::styled(
                    cursor_char.to_string(),
                    Style::default().fg(theme::THINKING),
                ),
                Span::styled(format!("[{}] ", exist_icon), exist_style),
                Span::styled(format!("{:<8} ", entry.label), style),
                Span::styled(path_display, Style::default().fg(theme::MUTED)),
            ]));

            // Show creation hint for missing file under cursor
            if !entry.exists && is_cursor {
                lines.push(Line::from(Span::styled(
                    "    Press Enter to create and edit",
                    Style::default().fg(theme::MUTED),
                )));
            }
        }

        let scroll_offset = self.nav.scroll_offset();
        let mut scroll_state = ScrollState::with_offset(scroll_offset);
        let _metrics = ScrollableArea::new(Text::from(lines))
            .scrollbar_style(Style::default().fg(theme::MUTED))
            .render(f, inner, &mut scroll_state);
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;
        match input {
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
            // Enter: open editor (TODO: spawn external editor)
            Input {
                key: Key::Enter, ..
            } => {
                // TODO: spawn external editor for the file at self.entries[self.cursor()].path
                // For now, just close the panel as a no-op placeholder.
                vec![PanelEffect::Close]
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
        // 2 entries * 2 (label + possible hint) + 4 (border/padding) = 8, min 6
        (self.entries.len() as u16 * 2 + 4).max(6)
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            (
                "\u{2191}\u{2193}".to_string(),
                lc.tr("key-select").to_string(),
            ),
            ("Enter".to_string(), lc.tr("key-edit").to_string()),
            ("Esc".to_string(), lc.tr("key-cancel").to_string()),
        ]
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

    #[test]
    fn test_kind_returns_memory() {
        let panel = MemoryPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Memory);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = MemoryPanel::empty();
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
    fn test_up_down_navigation() {
        let mut panel = MemoryPanel::new("/tmp".to_string(), None);
        let ctx = make_ctx();

        // Starts at 0
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

        // Down -> clamped at 1
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
    fn test_enter_returns_close_placeholder() {
        let mut panel = MemoryPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(
            Input {
                key: Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        // TODO: Once editor spawning is implemented, this should produce
        // a different effect. For now it closes as a placeholder.
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], PanelEffect::Close);
    }

    #[test]
    fn test_desired_height() {
        let panel = MemoryPanel::empty();
        // 2 entries * 2 + 4 = 8
        assert_eq!(panel.desired_height(50, 80), 8);
    }

    #[test]
    fn test_render_does_not_panic() {
        let mut panel = MemoryPanel::new("/tmp".to_string(), None);
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = MemoryPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 3);
    }
}
