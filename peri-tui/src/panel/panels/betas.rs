//! v2 BetasPanel -- Beta feature toggle panel (PanelState trait implementation).
//!
//! Displays a list of beta feature toggles. The user navigates with arrow keys,
//! toggles with Space/Left/Right, and closes with Esc. Currently empty (no
//! active beta features) but preserves the full UI skeleton.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use tui_textarea::Input;

use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::panel::PanelState;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::ui::theme;

// ---------------------------------------------------------------------------
// BetaEntry
// ---------------------------------------------------------------------------

/// A single beta feature toggle entry.
#[derive(Debug, Clone)]
struct BetaEntry {
    label: String,
    description: String,
    enabled: bool,
}

/// Beta feature key list (currently empty -- no active beta features).
const BETA_KEYS: &[&str] = &[];

// ---------------------------------------------------------------------------
// BetasPanel
// ---------------------------------------------------------------------------

/// v2 Beta feature toggle panel.
///
/// UI-local state only (entries, cursor). Side-effects (close panel) are
/// returned as `PanelEffect` values.
#[derive(Debug)]
pub struct BetasPanel {
    /// All beta toggle entries.
    entries: Vec<BetaEntry>,
    /// Current cursor index.
    cursor: usize,
}

impl BetasPanel {
    /// Construct an empty panel for the registry factory.
    pub fn empty() -> Self {
        let entries = BETA_KEYS
            .iter()
            .map(|&key| BetaEntry {
                label: key.to_string(),
                description: String::new(),
                enabled: false,
            })
            .collect();

        Self { entries, cursor: 0 }
    }

    /// Construct a panel from live App data.
    ///
    /// Currently delegates to `empty()` since there are no active beta
    /// features. When beta keys are added to `BETA_KEYS`, this can read
    /// their actual enabled state from `app.services.peri_config`.
    pub fn from_app(_app: &crate::app::App) -> Self {
        Self::empty()
    }

    /// Toggle the entry at the current cursor position.
    fn toggle_current(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.cursor) {
            entry.enabled = !entry.enabled;
        }
    }

    /// Cursor position (0-based index into entries).
    fn cursor(&self) -> usize {
        self.cursor
    }
}

impl PanelState for BetasPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Betas
    }

    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelReadContext) {
        let title = " Beta \u{529f}\u{80fd}\u{5f00}\u{5173} ";
        let inner = BorderedPanel::new(Span::styled(
            title,
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        let mut lines: Vec<Line> = Vec::new();

        // Top hint line
        lines.push(Line::from(Span::styled(
            "  \u{53d8}\u{66f4}\u{5c06}\u{5728}\u{65b0}\u{4f1a}\u{8bdd}\u{4e2d}\u{751f}\u{6548}",
            Style::default().fg(theme::MUTED),
        )));
        // Spacer
        lines.push(Line::from(""));

        let desc_style = Style::default().fg(theme::MUTED);

        for (i, entry) in self.entries.iter().enumerate() {
            let is_cursor = i == self.cursor();
            let cursor_char = if is_cursor { "\u{276f} " } else { "  " };
            let label_style = if is_cursor {
                Style::default()
                    .fg(theme::THINKING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };

            let value_style = if entry.enabled {
                Style::default()
                    .fg(theme::SAGE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::MUTED)
            };
            let value_text = if entry.enabled { "on" } else { "off" };

            lines.push(Line::from(vec![
                Span::styled(
                    cursor_char.to_string(),
                    Style::default().fg(theme::THINKING),
                ),
                Span::styled(format!("{:<14}", entry.label), label_style),
                Span::styled(value_text.to_string(), value_style),
            ]));
            lines.push(Line::from(Span::styled(
                format!("      {}", entry.description),
                desc_style,
            )));
        }

        // Empty hint when no entries
        if self.entries.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  \u{6682}\u{65e0}\u{6d3b}\u{8dc3}\u{7684} Beta \u{529f}\u{80fd}",
                Style::default().fg(theme::MUTED),
            )));
        }

        lines.truncate(inner.height as usize);
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;
        match input {
            // Up/Down: navigate
            Input { key: Key::Up, .. } => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                vec![]
            }
            Input { key: Key::Down, .. } => {
                if self.cursor < self.entries.len().saturating_sub(1) {
                    self.cursor += 1;
                }
                vec![]
            }
            // Space / Left / Right: toggle current entry
            Input {
                key: Key::Char(' '),
                ctrl: false,
                ..
            }
            | Input {
                key: Key::Left,
                ctrl: false,
                ..
            }
            | Input {
                key: Key::Right,
                ctrl: false,
                ..
            } => {
                self.toggle_current();
                vec![]
            }
            // Esc: close
            Input { key: Key::Esc, .. } => vec![PanelEffect::Close],
            // All other keys: consumed (no-op)
            _ => vec![],
        }
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        // Hint(1) + spacer(1) + per-entry 2 lines + empty-hint 2 + border(2)
        (self.entries.len() as u16 * 2 + 4).max(6)
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            (
                "\u{2191}\u{2193}".to_string(),
                lc.tr("key-select").to_string(),
            ),
            (
                "\u{2190}\u{2192}".to_string(),
                "\u{5207}\u{6362}".to_string(),
            ),
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

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tui_textarea::Key;

    use super::*;
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

    #[test]
    fn test_kind_returns_betas() {
        let panel = BetasPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Betas);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = BetasPanel::empty();
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
    fn test_up_down_navigation_clamped() {
        let mut panel = BetasPanel::empty();
        let ctx = make_ctx();

        // Empty list: cursor stays at 0
        assert_eq!(panel.cursor(), 0);
        panel.handle_key(
            Input {
                key: Key::Down,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert_eq!(panel.cursor(), 0);
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
    fn test_toggle_with_space() {
        let mut panel = BetasPanel::empty();
        let ctx = make_ctx();

        // Toggle on empty list: no panic, no effect
        let effects = panel.handle_key(
            Input {
                key: Key::Char(' '),
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert!(effects.is_empty());
    }

    #[test]
    fn test_desired_height_minimum() {
        let panel = BetasPanel::empty();
        // Empty list: 0*2 + 4 = 4 -> max(6) = 6
        assert_eq!(panel.desired_height(50, 80), 6);
    }

    #[test]
    fn test_render_does_not_panic() {
        let mut panel = BetasPanel::empty();
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = BetasPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 3);
    }
}
