//! Panel factory and stub placeholder.
//!
//! `create_panel(kind)` returns a `Box<dyn PanelState>` for the given
//! `PanelKind`. During P3 migration each arm switches from `PanelStateStub`
//! to the concrete implementation.

use ratatui::crossterm::event::MouseEvent;
use ratatui::layout::Rect;
use ratatui::Frame;
use tui_textarea::Input;

use super::{PanelEffect, PanelReadContext};
use crate::app::panel_manager::PanelKind;
use crate::i18n::LcRegistry;

// ---------------------------------------------------------------------------
// PanelStateStub
// ---------------------------------------------------------------------------

/// Temporary placeholder for panels not yet migrated to v2 `PanelState`.
///
/// Implements all trait methods with minimal behavior (empty render,
/// returns `Close` on any key). Each concrete panel migration replaces
/// the corresponding `create_panel` arm.
#[derive(Debug)]
pub struct PanelStateStub {
    kind: PanelKind,
}

impl PanelStateStub {
    /// Create a stub for the given panel kind.
    pub fn new(kind: PanelKind) -> Self {
        Self { kind }
    }
}

impl super::PanelState for PanelStateStub {
    fn kind(&self) -> PanelKind {
        self.kind
    }

    fn render(&mut self, _f: &mut Frame, _area: Rect, _ctx: &PanelReadContext) {
        // Stub: render nothing. Real panels will override this.
    }

    fn handle_key(&mut self, _input: Input, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        // Stub: immediately close so the user isn't stuck.
        vec![PanelEffect::Close]
    }

    fn handle_mouse(
        &mut self,
        _mouse: MouseEvent,
        _area: Rect,
        _ctx: &PanelReadContext,
    ) -> Vec<PanelEffect> {
        vec![]
    }

    fn handle_scroll(&mut self, _lines: i16, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        vec![]
    }

    fn handle_paste(&mut self, _text: &str, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        vec![]
    }

    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        20
    }

    fn status_bar_hints(&self, _lc: &LcRegistry) -> Vec<(String, String)> {
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create a `Box<dyn PanelState>` for the given `PanelKind`.
///
/// All 14 arms currently return `PanelStateStub`. During P3 migration,
/// each arm is replaced with the concrete panel implementation.
pub fn create_panel(kind: PanelKind) -> Box<dyn super::PanelState> {
    match kind {
        PanelKind::Model => Box::new(super::panels::model::ModelPanel::empty()),
        PanelKind::Login => Box::new(PanelStateStub::new(kind)),
        PanelKind::Agent => Box::new(super::panels::agent::AgentPanel::empty()),
        PanelKind::Hooks => Box::new(super::panels::hooks::HooksPanel::empty()),
        PanelKind::Config => Box::new(super::panels::config::ConfigPanel::empty()),
        PanelKind::ThreadBrowser => Box::new(PanelStateStub::new(kind)),
        PanelKind::Mcp => Box::new(PanelStateStub::new(kind)),
        PanelKind::Plugin => Box::new(PanelStateStub::new(kind)),
        PanelKind::Cron => Box::new(PanelStateStub::new(kind)),
        PanelKind::Status => Box::new(super::panels::status::StatusPanel::empty()),
        PanelKind::Memory => Box::new(super::panels::memory::MemoryPanel::empty()),
        PanelKind::Tasks => Box::new(PanelStateStub::new(kind)),
        PanelKind::Betas => Box::new(super::panels::betas::BetasPanel::empty()),
        PanelKind::Workflow => Box::new(PanelStateStub::new(kind)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::panel::read_context::ServiceRegistrySnapshot;
    use crate::panel::PanelState;

    #[test]
    fn test_create_panel_returns_correct_kind() {
        for kind in &[
            PanelKind::Model,
            PanelKind::Login,
            PanelKind::Agent,
            PanelKind::Hooks,
            PanelKind::Config,
            PanelKind::ThreadBrowser,
            PanelKind::Mcp,
            PanelKind::Plugin,
            PanelKind::Cron,
            PanelKind::Status,
            PanelKind::Memory,
            PanelKind::Tasks,
            PanelKind::Betas,
            PanelKind::Workflow,
        ] {
            let panel = create_panel(*kind);
            assert_eq!(
                panel.kind(),
                *kind,
                "create_panel({kind:?}) returned wrong kind"
            );
        }
    }

    #[test]
    fn test_stub_handle_key_returns_close() {
        thread_local! {
            static STUB_SNAPSHOT: ServiceRegistrySnapshot = ServiceRegistrySnapshot::new();
            static STUB_VMS: Vec<peri_acp_types::view_model::ViewModel> = const { Vec::new() };
            #[allow(clippy::missing_const_for_thread_local)]
            static STUB_CACHE: HashMap<String, serde_json::Value> = HashMap::new();
            static STUB_LC: LcRegistry = LcRegistry::default();
        }
        STUB_SNAPSHOT.with(|snapshot| {
            STUB_VMS.with(|vms| {
                STUB_CACHE.with(|cache| {
                    STUB_LC.with(|lc| {
                        let mut stub = PanelStateStub::new(PanelKind::Memory);
                        let ctx = PanelReadContext {
                            services: snapshot,
                            view_models: vms,
                            scroll_offset: 0,
                            area: Rect::new(0, 0, 80, 24),
                            lc,
                            acp_query_cache: cache,
                        };
                        let effects = stub.handle_key(Input::default(), &ctx);
                        assert_eq!(effects.len(), 1);
                        assert_eq!(effects[0], PanelEffect::Close);
                    })
                })
            })
        });
    }

    #[test]
    fn test_stub_render_does_not_panic() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        thread_local! {
            static STUB_SNAPSHOT: ServiceRegistrySnapshot = ServiceRegistrySnapshot::new();
            static STUB_VMS: Vec<peri_acp_types::view_model::ViewModel> = const { Vec::new() };
            #[allow(clippy::missing_const_for_thread_local)]
            static STUB_CACHE: HashMap<String, serde_json::Value> = HashMap::new();
            static STUB_LC: LcRegistry = LcRegistry::default();
        }
        STUB_SNAPSHOT.with(|snapshot| {
            STUB_VMS.with(|vms| {
                STUB_CACHE.with(|cache| {
                    STUB_LC.with(|lc| {
                        let mut stub = PanelStateStub::new(PanelKind::Model);
                        let ctx = PanelReadContext {
                            services: snapshot,
                            view_models: vms,
                            scroll_offset: 0,
                            area: Rect::new(0, 0, 80, 24),
                            lc,
                            acp_query_cache: cache,
                        };
                        let backend = TestBackend::new(80, 24);
                        let mut terminal = Terminal::new(backend).unwrap();
                        terminal
                            .draw(|f| stub.render(f, Rect::new(0, 0, 80, 20), &ctx))
                            .unwrap();
                    })
                })
            })
        });
    }
}
