//! v2 Panel trait infrastructure for Phase 3 panel migration.
//!
//! Defines the `PanelState` trait, `PanelReadContext` (read-only snapshot),
//! `PanelEffect` (restricted side-effects), `ServiceRegistrySnapshot`, and
//! the `create_panel` factory.
//!
//! All concrete panel implementations will live in sub-modules and register
//! via `registry::create_panel`. During P3 migration, each `PanelKind` arm
//! in the factory switches from `PanelStateStub` to the real implementation.

pub mod effect;
pub mod panels;
pub mod read_context;
pub mod registry;

// Re-export the primary types at the panel module level.
pub use effect::PanelEffect;
pub use read_context::{PanelReadContext, ServiceRegistrySnapshot};
pub use registry::create_panel;

use ratatui::crossterm::event::MouseEvent;
use ratatui::layout::Rect;
use ratatui::Frame;
use tui_textarea::Input;

use crate::app::panel_types::PanelKind;
use crate::i18n::LcRegistry;

/// Interface implemented by every v2 panel.
///
/// New panels only need to implement this trait -- no changes to the state
/// machine core. The state machine calls methods through `dyn PanelState`
/// inside `ModalState::Panel(Box<dyn PanelState>)`.
///
/// Methods with default implementations (mouse, scroll, paste, desired_height,
/// status_bar_hints) allow simple panels to implement only `kind`, `render`,
/// and `handle_key`.
pub trait PanelState: Send + std::fmt::Debug {
    /// Return the `PanelKind` of this panel.
    fn kind(&self) -> PanelKind;

    /// Render the panel into the given area of the terminal frame.
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelReadContext);

    /// Handle a single key event, returning zero or more effects.
    fn handle_key(&mut self, input: Input, ctx: &PanelReadContext) -> Vec<PanelEffect>;

    /// Handle a mouse event (click, hover, etc.). Default: no-op.
    fn handle_mouse(
        &mut self,
        _mouse: MouseEvent,
        _area: Rect,
        _ctx: &PanelReadContext,
    ) -> Vec<PanelEffect> {
        Vec::new()
    }

    /// Handle a scroll event (line count, positive = down). Default: no-op.
    fn handle_scroll(&mut self, _lines: i16, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        Vec::new()
    }

    /// Handle a paste event. Default: no-op.
    fn handle_paste(&mut self, _text: &str, _ctx: &PanelReadContext) -> Vec<PanelEffect> {
        Vec::new()
    }

    /// Desired panel height given screen dimensions. Default: 20.
    fn desired_height(&self, _screen_h: u16, _screen_w: u16) -> u16 {
        20
    }

    /// Status-bar shortcut hints. Default: empty.
    fn status_bar_hints(&self, _lc: &LcRegistry) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Refresh cached data from live `App` state. Default: no-op.
    ///
    /// Called by `draw_now` before every `render`. Caching panels — those
    /// that snapshot live data at `from_app` time (Workflow / Cron / Tasks
    /// / ThreadBrowser / Mcp / Plugin) — override this to pull fresh data
    /// so the panel doesn't show a stale snapshot while open.
    ///
    /// **Cursor / scroll preservation**: refresh must NOT reset cursor
    /// position or scroll offset — those represent user intent and would
    /// cause a jarring "jump back to top" every render. Use the panel's
    /// existing `set_*` methods if they clamp cursors; otherwise manually
    /// preserve.
    ///
    /// **Edit-buffer panels skip**: form-style panels (Model / Login /
    /// Config) hold user-edit buffers that MUST persist across renders —
    /// overriding refresh would clobber the user's typing. The default
    /// no-op is correct for them.
    ///
    /// **Cron #30**: prior to this hook, all 7 caching panels showed data
    /// frozen at open time. WorkflowPanel's progress never advanced; cron
    /// toggles weren't reflected locally; newly-created sessions didn't
    /// appear in ThreadBrowser; MCP connection status didn't update.
    fn refresh(&mut self, _app: &crate::app::App) {}
}
