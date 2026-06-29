//! Restricted set of side-effects a panel can produce.
//!
//! The state machine maps each variant to a standard `Effect` before the
//! main loop executes it. Panels never touch terminal, network, or clipboard
//! directly.

/// Side-effect instructions produced by a panel.
///
/// The state machine translates these into top-level `Effect`s (render,
/// ACP request, clipboard write, etc.).
#[derive(Debug, Clone, PartialEq)]
pub enum PanelEffect {
    /// Inject a system notification text into the message area.
    ShowNotification(String),

    /// Send a command/query to the ACP layer.
    SendToAcp {
        /// Event name (e.g. "query_cron_tasks").
        event: String,
        /// Payload data.
        data: serde_json::Value,
    },

    /// Close this panel.
    Close,

    /// Switch to another session by ID.
    SwitchSession(String),

    /// Copy text to the system clipboard.
    Copy(String),

    /// Update a configuration item (persisted + synced to ACP Server).
    UpdateConfig {
        /// Configuration key (e.g. "model_provider").
        key: String,
        /// New value (stringified).
        value: String,
    },
}
