//! Explicit capability negotiation for the ACP v2 stdio preview.
//!
//! The agent advertises only what this preview can honour; the client's
//! declaration is captured so peer requests can be gated instead of being sent
//! to a client that would answer Method not found.

use serde_json::{json, Value};

/// Client side declarations this preview acts on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ClientCapabilities {
    pub elicitation_form: bool,
    pub elicitation_url: bool,
}

impl ClientCapabilities {
    /// Capture the elicitation modes advertised in `initialize`.
    ///
    /// v2 names the client declaration `capabilities` (v1's `clientCapabilities`
    /// is not part of the pinned `InitializeRequest`). Per the pinned schema an
    /// absent or `null` mode is "not advertised" and an empty object advertises
    /// it; anything else is treated as not advertised so a malformed declaration
    /// cannot unlock a request the client cannot answer.
    pub(super) fn capture(params: &Value) -> Self {
        let elicitation = params
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("elicitation"));
        Self {
            elicitation_form: advertised(elicitation, "form"),
            elicitation_url: advertised(elicitation, "url"),
        }
    }

    /// Peri only ever sends form elicitations over this adapter.
    pub(super) fn supports_form_elicitation(self) -> bool {
        self.elicitation_form
    }
}

fn advertised(elicitation: Option<&Value>, mode: &str) -> bool {
    elicitation
        .and_then(|elicitation| elicitation.get(mode))
        .is_some_and(Value::is_object)
}

/// `initialize` result for a v2 connection.
///
/// Baseline session methods are implied by `session: {}`; `delete` is advertised
/// because S1 lands `session/delete`. Config controls (S3), authentication,
/// extra roots and system prompts are deliberately absent.
pub(super) fn initialize_response() -> Value {
    json!({
        "protocolVersion": 2,
        "info": {"name": "peri", "title": "Peri", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": {"session": {
            "prompt": {"image": {}},
            "mcp": {"stdio": {}, "http": {}},
            "delete": {}
        }}
    })
}

#[cfg(test)]
#[path = "capabilities_test.rs"]
mod tests;
