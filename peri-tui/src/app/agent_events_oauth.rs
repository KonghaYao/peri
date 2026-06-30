use super::{events::OAuthCallbackResult, *};

impl App {
    pub(crate) fn handle_oauth_needed(
        &mut self,
        server_name: String,
        authorization_url: String,
        callback_tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
    ) -> (bool, bool, bool) {
        self.global_ui.oauth_prompt = Some(OAuthPrompt::new(
            server_name,
            authorization_url,
            callback_tx,
        ));
        (true, true, false)
    }

    pub(crate) fn handle_oauth_completed(&mut self, server_name: String) -> (bool, bool, bool) {
        self.global_ui.oauth_prompt = None;
        self.push_system_note(self.services.lc.tr_args(
            "mcp-oauth-completed",
            &[("server".into(), server_name.into())],
        ));
        (true, false, false)
    }

    pub(crate) fn handle_oauth_failed(
        &mut self,
        server_name: String,
        error: String,
    ) -> (bool, bool, bool) {
        self.global_ui.oauth_prompt = None;
        self.push_system_note(self.services.lc.tr_args(
            "mcp-oauth-failed",
            &[
                ("server".into(), server_name.into()),
                ("error".into(), error.into()),
            ],
        ));
        (true, false, false)
    }

    pub(crate) fn handle_mcp_action_completed(
        &mut self,
        server_name: String,
        action: String,
        success: bool,
    ) -> (bool, bool, bool) {
        let msg = match (action.as_str(), success) {
            ("clear_auth", true) => self.services.lc.tr_args(
                "mcp-clear-auth-ok",
                &[("server".into(), server_name.clone().into())],
            ),
            ("clear_auth", false) => self.services.lc.tr_args(
                "mcp-clear-auth-failed",
                &[("server".into(), server_name.clone().into())],
            ),
            (_, true) => self.services.lc.tr_args(
                "mcp-action-ok",
                &[("server".into(), server_name.clone().into())],
            ),
            (_, false) => self.services.lc.tr_args(
                "mcp-action-failed",
                &[("server".into(), server_name.clone().into())],
            ),
        };
        self.push_system_note(msg);
        (true, false, false)
    }
}
