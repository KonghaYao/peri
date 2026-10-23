use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::session::InboxHandle;
use peri_acp_types::tasks::TaskManager;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExternalTaskRoute {
    pub(crate) server: String,
    pub(crate) raw_id: String,
}

#[derive(Default)]
struct Binding {
    routes: HashMap<String, ExternalTaskRoute>,
    inbox: Option<InboxHandle>,
    manager: Option<Arc<dyn TaskManager>>,
    registered: bool,
}

#[derive(Default)]
pub(crate) struct SessionBindings {
    current: HashMap<String, Binding>,
}

impl SessionBindings {
    pub(crate) fn record_external_task_route(
        &mut self,
        session: &str,
        public_id: &str,
        server: &str,
        raw_id: &str,
    ) -> Result<(), String> {
        let binding = self
            .current
            .get_mut(session)
            .ok_or("unknown task for this session")?;
        let route = ExternalTaskRoute {
            server: server.into(),
            raw_id: raw_id.into(),
        };
        if binding
            .routes
            .get(public_id)
            .is_some_and(|existing| existing != &route)
        {
            return Err("conflicting external task route".into());
        }
        binding.routes.insert(public_id.into(), route);
        Ok(())
    }

    pub(crate) fn resolve_external_task_route(
        &self,
        session: &str,
        public_id: &str,
        server: &str,
    ) -> Result<ExternalTaskRoute, String> {
        let route = self
            .current
            .get(session)
            .and_then(|binding| binding.routes.get(public_id))
            .ok_or("unknown task for this session")?;
        if route.server != server {
            return Err("task belongs to a different server".into());
        }
        Ok(route.clone())
    }

    #[cfg(test)]
    pub(crate) fn registered_sessions(&self) -> Vec<String> {
        self.current
            .keys()
            .filter(|session| self.inbox(session).is_some())
            .cloned()
            .collect()
    }

    pub(crate) fn verify_environment_close(&self, root: &str) -> Result<(), String> {
        for (session, binding) in &self.current {
            if session == root {
                continue;
            }
            let manager = binding
                .manager
                .as_ref()
                .ok_or_else(|| format!("Incomplete: child {session} ownership unknown"))?;
            let inbox = binding
                .inbox
                .as_ref()
                .ok_or_else(|| format!("Incomplete: child {session} inbox unknown"))?;
            if !manager.is_execution_idle() || inbox.queue().has_required() {
                return Err(format!(
                    "Incomplete: child {session} resources or required messages unfinished"
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn binding(&self, session: &str) -> Option<(InboxHandle, Arc<dyn TaskManager>)> {
        let binding = self.current.get(session)?;
        Some((binding.inbox.clone()?, binding.manager.clone()?))
    }

    pub(crate) fn manager(&self, session: &str) -> Option<Arc<dyn TaskManager>> {
        self.current.get(session)?.manager.clone()
    }

    pub(crate) fn inbox(&self, session: &str) -> Option<InboxHandle> {
        let binding = self.current.get(session)?;
        binding.registered.then(|| binding.inbox.clone()).flatten()
    }

    pub(crate) fn inboxes(&self) -> Vec<InboxHandle> {
        self.current
            .keys()
            .filter_map(|session| self.inbox(session))
            .collect()
    }

    pub(crate) fn bind_initial_manager(&mut self, session: &str, manager: Arc<dyn TaskManager>) {
        self.current.entry(session.into()).or_default().manager = Some(manager);
    }

    pub(crate) fn register_initial_inbox(&mut self, session: &str, inbox: InboxHandle) {
        let binding = self.current.entry(session.into()).or_default();
        binding.inbox = Some(inbox);
        binding.registered = true;
    }

    pub(crate) fn unregister(&mut self, session: &str) {
        if let Some(binding) = self.current.get_mut(session) {
            binding.registered = false;
        }
    }

    /// 已确认任务与消息结清后的子会话释放。
    pub(crate) fn release(&mut self, session: &str) {
        self.current.remove(session);
    }
}

#[cfg(test)]
#[path = "session_bindings_test.rs"]
mod tests;
