use std::sync::Arc;

use peri_acp_types::tasks::TaskManager;
use rmcp::model::RequestMetaObject;

use super::McpClientPool;

impl McpClientPool {
    pub fn bind_session_task_manager(&self, session_id: &str, manager: &Arc<dyn TaskManager>) {
        self.session_tasks
            .write()
            .insert(session_id.to_owned(), Arc::clone(manager));
        self.task_scope_tokens
            .write()
            .entry(session_id.to_owned())
            .or_insert_with(|| self.task_scope_authority.issue(session_id));
    }

    pub(crate) fn task_scope_meta_for(
        &self,
        server: &str,
        session_id: &str,
    ) -> Option<RequestMetaObject> {
        let token = if self.clients.read().get(server).is_some_and(|client| {
            matches!(
                client.source.as_ref(),
                Some(crate::mcp::config::ConfigSource::WorkspaceRemote)
            )
        }) {
            peri_mcp_core::task_scope::TaskScopeAuthority::trusted_connection().issue(session_id)
        } else {
            self.task_scope_tokens
                .write()
                .entry(session_id.to_owned())
                .or_insert_with(|| self.task_scope_authority.issue(session_id))
                .clone()
        };
        let mut meta = RequestMetaObject::new();
        meta.0 .0.insert(
            peri_mcp_core::task_scope::TASK_SCOPE_META_KEY.into(),
            token.into(),
        );
        Some(meta)
    }
}
