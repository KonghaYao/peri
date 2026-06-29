//! v2 StatusPanel -- Cost & Context status display panel (PanelState trait implementation).
//!
//! Displays token usage, cost estimates, and context charts in two tabs (Cost / Context).
//!
//! **NOTE (P3 migration)**: The legacy render reads session-level data (token tracker,
//! session start time, model name, context_window) from `App` which is not available
//! through `PanelReadContext`. Once `ServiceRegistrySnapshot` gains session snapshot
//! fields (e.g. `SessionStatusDto`), the render method should be updated to display
//! real data instead of the placeholder text.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_textarea::Input;

use peri_widgets::BorderedPanel;

use crate::app::panel_types::PanelKind;
use crate::panel::effect::PanelEffect;
use crate::panel::read_context::PanelReadContext;
use crate::panel::PanelState;
use crate::ui::theme;

/// Tab index constants (matching legacy STATUS_TAB_COST / STATUS_TAB_CONTEXT).
const TAB_COST: usize = 0;
const TAB_CONTEXT: usize = 1;

/// v2 Status panel.
///
/// UI-local state only (active tab, scroll). Display data will be injected via
/// `PanelReadContext` once `ServiceRegistrySnapshot` is extended with session
/// status fields.
#[derive(Debug)]
pub struct StatusPanel {
    /// Currently active tab (0 = Cost, 1 = Context).
    active_tab: usize,
    /// Provider name from app services (stored for future render use).
    #[allow(dead_code)]
    provider_name: String,
    /// Model name from app services (stored for future render use).
    #[allow(dead_code)]
    model_name: String,
}

impl StatusPanel {
    /// Construct an empty panel for the registry factory.
    /// Defaults to the Cost tab with empty provider/model names.
    pub fn empty() -> Self {
        Self {
            active_tab: TAB_COST,
            provider_name: String::new(),
            model_name: String::new(),
        }
    }

    /// Construct a panel from the live `App` state.
    pub fn from_app(app: &crate::app::App) -> Self {
        Self {
            active_tab: TAB_COST,
            provider_name: app.services.provider_name.clone(),
            model_name: app.services.model_name.clone(),
        }
    }
}

impl PanelState for StatusPanel {
    fn kind(&self) -> PanelKind {
        PanelKind::Status
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext) {
        let lc = ctx.lc;

        let inner = BorderedPanel::new(Span::styled(
            lc.tr("status-panel-title"),
            Style::default()
                .fg(theme::THINKING)
                .add_modifier(Modifier::BOLD),
        ))
        .border_style(Style::default().fg(theme::BORDER))
        .render(f, area);

        if inner.height < 3 {
            return;
        }

        // Tab bar (1 row)
        let tab_height = 1u16;
        let tab_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: tab_height,
        };
        let content_area = Rect {
            x: inner.x,
            y: inner.y + tab_height + 1,
            width: inner.width,
            height: inner.height.saturating_sub(tab_height + 1),
        };

        let tab_labels: Vec<Span> = [lc.tr("status-tab-cost"), lc.tr("status-tab-context")]
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let is_active = self.active_tab == i;
                let style = if is_active {
                    Style::default()
                        .fg(theme::TEXT)
                        .bg(theme::THINKING)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::MUTED)
                };
                Span::styled(format!(" {} ", label), style)
            })
            .collect();
        f.render_widget(Paragraph::new(Line::from(tab_labels)), tab_area);

        match self.active_tab {
            TAB_COST => {
                let mut lines: Vec<Line> = Vec::new();

                // Current model info (from App's ServiceRegistry).
                let model_label = format!(
                    "{}: {} / {}",
                    lc.tr("status-label-current-model"),
                    self.provider_name,
                    self.model_name,
                );
                lines.push(Line::from(Span::styled(
                    model_label,
                    Style::default()
                        .fg(theme::TEXT)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));

                // Per-session token stats require token tracker data
                // injected via PanelReadContext (future P5 enhancement).
                lines.push(Line::from(Span::styled(
                    lc.tr("status-empty-data"),
                    Style::default().fg(theme::MUTED),
                )));
                f.render_widget(Paragraph::new(lines), content_area);
            }
            TAB_CONTEXT => {
                let mut lines: Vec<Line> = Vec::new();

                // Model context window info.
                let context_info =
                    if self.model_name.contains("1m") || self.model_name.contains("1M") {
                        "1M tokens (extended)"
                    } else {
                        "200K tokens (standard)"
                    };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{}: ", lc.tr("status-label-context")),
                        Style::default().fg(theme::MUTED),
                    ),
                    Span::styled(context_info, Style::default().fg(theme::TEXT)),
                ]));
                lines.push(Line::from(""));

                // Per-session context usage requires live session data
                // injected via PanelReadContext (future P5 enhancement).
                lines.push(Line::from(Span::styled(
                    lc.tr("status-empty-data"),
                    Style::default().fg(theme::MUTED),
                )));
                f.render_widget(Paragraph::new(lines), content_area);
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        use tui_textarea::Key;
        match input {
            Input { key: Key::Esc, .. } => vec![PanelEffect::Close],
            Input { key: Key::Left, .. } => {
                self.active_tab = TAB_COST;
                vec![]
            }
            Input {
                key: Key::Right, ..
            } => {
                self.active_tab = TAB_CONTEXT;
                vec![]
            }
            _ => vec![],
        }
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        20
    }

    fn status_bar_hints(&self, lc: &crate::i18n::LcRegistry) -> Vec<(String, String)> {
        vec![
            ("\u{2190}\u{2192}".to_string(), lc.tr("key-tab")),
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
    fn test_kind_returns_status() {
        let panel = StatusPanel::empty();
        assert_eq!(panel.kind(), PanelKind::Status);
    }

    #[test]
    fn test_esc_close() {
        let mut panel = StatusPanel::empty();
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
    fn test_left_right_switches_tab() {
        let mut panel = StatusPanel::empty();
        let ctx = make_ctx();

        // Default tab is COST (0)
        assert_eq!(panel.active_tab, TAB_COST);

        // Right -> CONTEXT
        let effects = panel.handle_key(
            Input {
                key: Key::Right,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert!(effects.is_empty());
        assert_eq!(panel.active_tab, TAB_CONTEXT);

        // Left -> back to COST
        let effects = panel.handle_key(
            Input {
                key: Key::Left,
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert!(effects.is_empty());
        assert_eq!(panel.active_tab, TAB_COST);
    }

    #[test]
    fn test_other_keys_consumed_noop() {
        let mut panel = StatusPanel::empty();
        let ctx = make_ctx();
        let effects = panel.handle_key(
            Input {
                key: Key::Char('a'),
                ctrl: false,
                alt: false,
                shift: false,
            },
            &ctx,
        );
        assert!(effects.is_empty());
    }

    #[test]
    fn test_desired_height() {
        let panel = StatusPanel::empty();
        assert_eq!(panel.desired_height(50, 80), 20);
    }

    #[test]
    fn test_status_bar_hints() {
        let panel = StatusPanel::empty();
        let lc = crate::i18n::LcRegistry::default();
        let hints = panel.status_bar_hints(&lc);
        assert_eq!(hints.len(), 2);
    }

    #[test]
    fn test_render_does_not_panic() {
        let mut panel = StatusPanel::empty();
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }

    #[test]
    fn test_render_context_tab_does_not_panic() {
        let mut panel = StatusPanel::empty();
        panel.active_tab = TAB_CONTEXT;
        let ctx = make_ctx();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| panel.render(f, Rect::new(0, 0, 80, 20), &ctx))
            .unwrap();
    }
}
