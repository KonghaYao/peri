//! Preview-only configuration snapshot for the ACP v2 stdio adapter.
//!
//! `session/new` and `session/load` already return `configOptions`; a plain
//! `session/resume` reply does not. The v2 adapter asks for the same list here
//! instead of the host growing a second resume shape, so a resumed session has
//! the same configuration surface as a fresh one. The single construction
//! authority stays [`crate::dispatch::config_update::make_config_options`].

use serde_json::{json, Value};

use super::ServerLoop;
use crate::host::stdio::v2::errors;
use crate::transport::types::{AcpError, RequestId};

impl ServerLoop<'_> {
    pub(super) async fn handle_stdio_preview_config(&self, id: RequestId, params: Value) {
        let result = self.stdio_preview_config(&params).await;
        let _ = self.transport.send_response(id, result).await;
    }

    async fn stdio_preview_config(&self, params: &Value) -> Result<Value, AcpError> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| errors::invalid_params("Missing sessionId"))?;
        // A session assembled from its own environment configuration owns its
        // option list; anything else answers from the connection's view.
        let environment = {
            let sessions = self.sessions.lock().await;
            let state = sessions
                .get(session_id)
                .filter(|state| !state.closing && state.close_in_flight == 0)
                .ok_or_else(|| errors::session_unavailable("Session not active"))?;
            state.environment.clone()
        };
        let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(self.cfg);
        let options = {
            let peri_config = cfg.peri_config.read();
            let provider = cfg.provider.read();
            crate::dispatch::config_update::make_config_options(
                &peri_config,
                &provider,
                cfg.permission_mode.load(),
            )
        };
        serde_json::to_value(json!({ "configOptions": options }))
            .map_err(|error| errors::internal(error.to_string()))
    }
}
