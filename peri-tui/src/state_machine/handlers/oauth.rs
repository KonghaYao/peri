//! OAuth authorisation handler.
//!
//! Wraps an [`peri_acp_types::event_data::OauthNeeded`] payload. The P2 stub
//! implements [`crate::state_machine::state::Handler`] with no real key
//! dispatch -- the actual OAuth-flow UI logic lands in P3.

use peri_acp_types::event_data::OauthNeeded;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::super::state::{Handler, HandlerOutput};

/// Handler for an `"oauth-needed"` event. Holds the OAuth request.
#[derive(Debug)]
pub struct OauthHandler {
    /// The OAuth authorisation request received from the ACP layer.
    pub request: OauthNeeded,
    /// Track whether the user chose to open the auth URL.
    url_opened: bool,
}

impl OauthHandler {
    /// Create a new handler from an oauth-needed payload.
    pub fn new(request: OauthNeeded) -> Self {
        Self {
            request,
            url_opened: false,
        }
    }
}

impl Handler for OauthHandler {
    fn render(&self, _frame: &mut ratatui::Frame, _area: ratatui::layout::Rect) {
        // Phase 1.3: render OAuth popup (server name + auth URL + open/close
        // hints). For now the legacy popup system handles it.
    }

    fn handle_key(&mut self, key: KeyEvent) -> HandlerOutput {
        match key.code {
            KeyCode::Enter => {
                // Open the auth URL in browser
                self.url_opened = true;
                HandlerOutput::Submit(self.request.auth_url.clone())
            }
            KeyCode::Char('o' | 'O') => {
                self.url_opened = true;
                HandlerOutput::Submit(self.request.auth_url.clone())
            }
            KeyCode::Esc => HandlerOutput::Dismiss,
            KeyCode::Char('q' | 'Q' | 'c' | 'C') => HandlerOutput::Dismiss,
            _ => HandlerOutput::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::handler::{key, key_enter, key_esc};
    use super::*;

    fn make_request() -> OauthNeeded {
        OauthNeeded {
            server_name: "github-mcp".into(),
            auth_url: "https://github.com/login/oauth".into(),
        }
    }

    #[test]
    fn test_handle_key_enter_opens_url() {
        let mut h = OauthHandler::new(make_request());
        let output = h.handle_key(key_enter());
        assert!(matches!(output, HandlerOutput::Submit(ref s) if s.contains("github.com")));
    }

    #[test]
    fn test_handle_key_o_opens_url() {
        let mut h = OauthHandler::new(make_request());
        assert!(matches!(h.handle_key(key('o')), HandlerOutput::Submit(_)));
    }

    #[test]
    fn test_handle_key_esc_dismisses() {
        let mut h = OauthHandler::new(make_request());
        assert_eq!(h.handle_key(key_esc()), HandlerOutput::Dismiss);
    }

    #[test]
    fn test_handle_key_q_dismisses() {
        let mut h = OauthHandler::new(make_request());
        assert_eq!(h.handle_key(key('q')), HandlerOutput::Dismiss);
    }
}
