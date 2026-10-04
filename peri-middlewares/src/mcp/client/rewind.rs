use std::time::Duration;

use rmcp::{
    model::{ClientRequest, CustomRequest, ServerResult},
    service::PeerRequestOptions,
};

use super::{ClientStatus, McpClientPool};

impl McpClientPool {
    pub(super) async fn rewind_workspace_files(
        &self,
        session_id: &str,
        changes: serde_json::Value,
    ) -> Result<(), String> {
        if !self.is_open() {
            return Err("Workspace capability is closed".into());
        }
        if self.builtin_instance_context().is_some_and(|context| {
            let source = self
                .get_client("workspace")
                .and_then(|handle| handle.source.clone());
            crate::mcp::builtin::is_closed_source("workspace", source.as_ref(), &context.closed)
        }) {
            return Err("Workspace capability is closed".into());
        }
        let handle = self
            .get_client_visible_to("workspace", Some(session_id))
            .filter(|handle| matches!(handle.status, ClientStatus::Connected))
            .filter(|handle| crate::mcp::builtin::is_workspace_source(handle.source.as_ref()))
            .ok_or("trusted Workspace capability unavailable")?;
        let peer = handle.peer.as_ref().ok_or("Workspace disconnected")?;
        let generation = self.handle_generation(&handle);
        let scope = self
            .task_scope_meta_for("workspace", session_id)
            .ok_or("Workspace session scope unavailable")?;
        let request = CustomRequest::new(
            "workspace/rewindFiles",
            Some(serde_json::json!({"changes": changes, "_meta": scope})),
        );
        let result = peri_time::timeout(Duration::from_secs(30), async {
            let pending = peer
                .send_request_with_option(
                    ClientRequest::CustomRequest(request),
                    PeerRequestOptions::no_options(),
                )
                .await
                .map_err(|error| error.to_string())?;
            pending
                .await_response()
                .await
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|_| "Workspace rewind timed out; outcome unknown".to_string())??;
        if !self.is_open()
            || !self
                .get_client_visible_to("workspace", Some(session_id))
                .is_some_and(|current| {
                    std::sync::Arc::ptr_eq(&current, &handle)
                        && matches!(current.status, ClientStatus::Connected)
                        && self.handle_generation(&current) == generation
                })
        {
            return Err("Workspace owner changed during rewind; outcome unknown".into());
        }
        match result {
            ServerResult::CustomResult(result)
                if result.0.get("ok") == Some(&serde_json::Value::Bool(true)) =>
            {
                Ok(())
            }
            _ => Err("invalid Workspace rewind response".into()),
        }
    }
}
