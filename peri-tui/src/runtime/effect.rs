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
}
