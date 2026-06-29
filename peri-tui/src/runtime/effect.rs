/// Side-effect instruction produced by the state machine, executed by the main loop.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Render current state. (Snapshot is read from state in P2; for P1 thin-shell we re-render on each effect.)
    Render,
    /// Send an ACP method call or custom event upstream.
    SendToAcp {
        method: String,
        params: serde_json::Value,
    },
    /// Write to system clipboard.
    CopyToClipboard(String),
    /// Exit the app.
    Quit,
    /// Show a transient notification in the message area.
    /// Produced by `PanelEffect::ShowNotification`. Handled by main_loop
    /// because it needs `&mut App` (not just ApplyContext's I/O handles).
    ShowNotification(String),
    /// Update a configuration key-value pair.
    /// Produced by `PanelEffect::UpdateConfig`. Persisted to PeriConfig + synced
    /// to ACP Server. Handled by main_loop (needs App).
    UpdateConfig { key: String, value: String },
    /// Switch to another session by ID.
    /// Produced by `PanelEffect::SwitchSession`. Handled by main_loop (needs App).
    SwitchSession(String),
}
