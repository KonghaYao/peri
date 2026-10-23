use std::sync::Arc;

use peri_acp_types::tasks::TaskManager;
use rmcp::model::RequestMetaObject;

use super::McpClientPool;

/// Whether `source` is a Workspace owner that isolates background tasks by the
/// direct initiating session.
///
/// Other MCP servers remain generic: they never receive Peri task metadata.
fn workspace_task_owner(source: Option<&crate::mcp::config::ConfigSource>) -> bool {
    matches!(
        source,
        Some(crate::mcp::config::ConfigSource::WorkspaceRemote)
    ) || matches!(
        source,
        Some(crate::mcp::config::ConfigSource::Builtin { instance }) if instance == "workspace"
    )
}

/// Build the optional `_meta["peri/taskOwner"]` for a Workspace request.
///
/// The owner is the session that directly initiated the work. It is a plain
/// identity, not a secret or capability: remote deployments must authenticate
/// the connection itself.
pub(crate) fn task_owner_meta(
    source: Option<&crate::mcp::config::ConfigSource>,
    session_id: &str,
) -> Option<RequestMetaObject> {
    workspace_task_owner(source).then(|| {
        let mut meta = RequestMetaObject::new();
        meta.0 .0.insert(
            peri_mcp_core::task_owner::TASK_OWNER_META_KEY.into(),
            session_id.into(),
        );
        meta
    })
}

impl McpClientPool {
    pub fn bind_session_task_manager(&self, session_id: &str, manager: &Arc<dyn TaskManager>) {
        self.session_bindings
            .write()
            .bind_initial_manager(session_id, Arc::clone(manager));
    }

    pub(crate) fn task_owner_meta_for(
        &self,
        server: &str,
        session_id: &str,
    ) -> Option<RequestMetaObject> {
        let source = self
            .clients
            .read()
            .get(server)
            .and_then(|client| client.source.clone());
        task_owner_meta(source.as_ref(), session_id)
    }
}
