//! Panel factory and stub placeholder.
//!
//! `create_panel(kind)` returns a `Box<dyn PanelState>` for the given
//! `PanelKind`. During P3 migration each arm switches from `PanelStateStub`
//! to the concrete implementation.

use crate::app::panel_types::PanelKind;

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create a `Box<dyn PanelState>` for the given `PanelKind`.
///
/// Accepts `&App` so panels can be initialized with live data from
/// `ServiceRegistry` (MCP servers, cron tasks, config, etc.).
pub fn create_panel(kind: PanelKind, app: &crate::app::App) -> Box<dyn super::PanelState> {
    match kind {
        PanelKind::Model => Box::new(super::panels::model::ModelPanel::from_app(app)),
        PanelKind::Login => Box::new(super::panels::login::LoginPanel::from_app(app)),
        PanelKind::Agent => Box::new(super::panels::agent::AgentPanel::from_app(app)),
        PanelKind::Hooks => Box::new(super::panels::hooks::HooksPanel::from_app(app)),
        PanelKind::Config => Box::new(super::panels::config::ConfigPanel::from_app(app)),
        PanelKind::ThreadBrowser => Box::new(
            super::panels::thread_browser::ThreadBrowserPanel::from_app(app),
        ),
        PanelKind::Mcp => Box::new(super::panels::mcp::McpPanel::from_app(app)),
        PanelKind::Plugin => Box::new(super::panels::plugin::PluginPanel::from_app(app)),
        PanelKind::Cron => Box::new(super::panels::cron::CronPanel::from_app(app)),
        PanelKind::Status => Box::new(super::panels::status::StatusPanel::from_app(app)),
        PanelKind::Memory => Box::new(super::panels::memory::MemoryPanel::from_app(app)),
        PanelKind::Tasks => Box::new(super::panels::tasks::TasksPanel::from_app(app)),
        PanelKind::Betas => Box::new(super::panels::betas::BetasPanel::from_app(app)),
        PanelKind::Workflow => Box::new(super::panels::workflow::WorkflowPanel::from_app(app)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_panel_returns_correct_kind() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let app = rt.block_on(async { crate::app::App::new_headless(80, 24).await.0 });
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
            let panel = create_panel(*kind, &app);
            assert_eq!(
                panel.kind(),
                *kind,
                "create_panel({kind:?}) returned wrong kind"
            );
        }
    }
}
