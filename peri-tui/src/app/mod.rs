// ── State ─────────────────────────────────────────────────────────────────────
mod global_ui_state;
pub mod service_registry;
pub use global_ui_state::GlobalUiState;
pub use service_registry::ServiceRegistry;

// ── Provider ──────────────────────────────────────────────────────────────────
pub mod agent;
pub use agent::LlmProvider;

// ── UI Interaction ────────────────────────────────────────────────────────────
pub mod panel_types;
pub use panel_types::PanelKind;

pub mod setup_wizard;

// ── Services ───────────────────────────────────────────────────────────────────
mod provider;

#[expect(
    clippy::module_inception,
    reason = "The style guide keeps the facade declarative and names the implementation leaf after its primary type."
)]
mod app;
use self::app as application;
pub use application::App;
