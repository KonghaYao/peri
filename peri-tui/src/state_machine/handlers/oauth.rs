//! OAuth authorisation handler.
//!
//! Wraps an [`peri_acp_types::event_data::OauthNeeded`] payload. The P2 stub
//! implements [`crate::state_machine::state::Handler`] with no real key
//! dispatch -- the actual OAuth-flow UI logic lands in P3.

use peri_acp_types::event_data::OauthNeeded;

use super::super::state::{Handler, HandlerOutput};

/// Handler for an `"oauth-needed"` event. Holds the OAuth request.
#[derive(Debug)]
pub struct OauthHandler {
    /// The OAuth authorisation request received from the ACP layer.
    pub request: OauthNeeded,
}

impl OauthHandler {
    /// Create a new handler from an oauth-needed payload.
    pub fn new(request: OauthNeeded) -> Self {
        Self { request }
    }
}

impl Handler for OauthHandler {
    fn render(&self, _area: (u16, u16)) {}

    fn handle_key(&mut self, _key: char) -> HandlerOutput {
        // P3 will dispatch Enter to open the auth URL / Esc to dismiss.
        HandlerOutput::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request() -> OauthNeeded {
        OauthNeeded {
            server_name: "github-mcp".into(),
            auth_url: "https://github.com/login/oauth".into(),
        }
    }

    #[test]
    fn test_handler_stores_payload() {
        let h = OauthHandler::new(make_request());
        assert_eq!(h.request.server_name, "github-mcp");
    }

    #[test]
    fn test_handle_key_returns_nothing() {
        let mut h = OauthHandler::new(make_request());
        assert_eq!(h.handle_key('o'), HandlerOutput::Nothing);
    }
}
