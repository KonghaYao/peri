use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::session::InboxHandle;
use peri_acp_types::tasks::TaskManager;

#[derive(Default)]
struct Binding {
    resources: Option<Arc<dyn peri_acp_types::session_resources::SessionResources>>,
    inbox: Option<InboxHandle>,
    manager: Option<Arc<dyn TaskManager>>,
    registered: bool,
}

#[derive(Default)]
pub(crate) struct SessionBindings {
    current: HashMap<String, u64>,
    history: HashMap<(String, u64), Binding>,
}

impl SessionBindings {
    pub(crate) fn resources(
        &self,
        session: &str,
        lifecycle: u64,
    ) -> Option<Arc<dyn peri_acp_types::session_resources::SessionResources>> {
        self.history
            .get(&(session.into(), lifecycle))?
            .resources
            .clone()
    }

    pub(crate) fn bind_resources(
        &mut self,
        session: &str,
        lifecycle: u64,
        resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
    ) -> Result<(), String> {
        let binding = self
            .history
            .get_mut(&(session.into(), lifecycle))
            .ok_or("Incomplete: exact MCP session lifecycle binding unavailable")?;
        binding.resources = Some(resources);
        Ok(())
    }

    pub(crate) fn binding_at(
        &self,
        session: &str,
        lifecycle: u64,
    ) -> Option<(InboxHandle, Arc<dyn TaskManager>)> {
        let binding = self.history.get(&(session.into(), lifecycle))?;
        Some((binding.inbox.clone()?, binding.manager.clone()?))
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
        for ((session, lifecycle), binding) in &self.history {
            if session == root {
                continue;
            }
            let manager = binding.manager.as_ref().ok_or_else(|| {
                format!("Incomplete: child {session}:{lifecycle} ownership unknown")
            })?;
            let inbox = binding
                .inbox
                .as_ref()
                .ok_or_else(|| format!("Incomplete: child {session}:{lifecycle} inbox unknown"))?;
            if !manager.is_execution_idle() || inbox.queue().has_required() {
                return Err(format!("Incomplete: child {session}:{lifecycle} resources or required messages unfinished"));
            }
        }
        Ok(())
    }
    pub(crate) fn lifecycle(&self, session: &str) -> Option<u64> {
        self.current.get(session).copied()
    }

    pub(crate) fn binding(&self, session: &str) -> Option<(InboxHandle, Arc<dyn TaskManager>)> {
        let lifecycle = self.lifecycle(session)?;
        let binding = self.history.get(&(session.into(), lifecycle))?;
        Some((binding.inbox.clone()?, binding.manager.clone()?))
    }

    pub(crate) fn manager(&self, session: &str) -> Option<Arc<dyn TaskManager>> {
        let lifecycle = self.lifecycle(session)?;
        self.history
            .get(&(session.into(), lifecycle))?
            .manager
            .clone()
    }

    pub(crate) fn inbox(&self, session: &str) -> Option<InboxHandle> {
        let lifecycle = self.lifecycle(session)?;
        let binding = self.history.get(&(session.into(), lifecycle))?;
        binding.registered.then(|| binding.inbox.clone()).flatten()
    }

    pub(crate) fn inboxes(&self) -> Vec<InboxHandle> {
        self.current
            .keys()
            .filter_map(|session| self.inbox(session))
            .collect()
    }

    pub(crate) fn bind(
        &mut self,
        session: &str,
        lifecycle: u64,
        inbox: InboxHandle,
        manager: Arc<dyn TaskManager>,
    ) -> Result<(), String> {
        if self
            .lifecycle(session)
            .is_some_and(|current| current > lifecycle)
        {
            return Err("Incomplete: stale session lifecycle binding".into());
        }
        if let Some(existing) = self.history.get(&(session.into(), lifecycle)) {
            if existing
                .manager
                .as_ref()
                .is_some_and(|old| !Arc::ptr_eq(old, &manager))
            {
                return Err("Incomplete: session lifecycle already has an execution owner".into());
            }
        }
        let resources = self
            .history
            .get(&(session.into(), lifecycle))
            .and_then(|binding| binding.resources.clone());
        self.history.insert(
            (session.into(), lifecycle),
            Binding {
                resources,
                inbox: Some(inbox),
                manager: Some(manager),
                registered: true,
            },
        );
        self.current.insert(session.into(), lifecycle);
        Ok(())
    }

    pub(crate) fn bind_initial_manager(&mut self, session: &str, manager: Arc<dyn TaskManager>) {
        let lifecycle = *self.current.entry(session.into()).or_insert(1);
        let binding = self.history.entry((session.into(), lifecycle)).or_default();
        if lifecycle != 1
            && binding
                .manager
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &manager))
        {
            tracing::error!(
                session,
                lifecycle,
                "legacy binding cannot replace typed lifecycle owner"
            );
            return;
        }
        binding.manager = Some(manager);
    }

    pub(crate) fn register_initial_inbox(&mut self, session: &str, inbox: InboxHandle) {
        let lifecycle = *self.current.entry(session.into()).or_insert(1);
        let binding = self.history.entry((session.into(), lifecycle)).or_default();
        if lifecycle != 1 {
            tracing::error!(
                session,
                lifecycle,
                "legacy registration cannot replace typed lifecycle inbox"
            );
            return;
        }
        binding.inbox = Some(inbox);
        binding.registered = true;
    }

    pub(crate) fn unregister(&mut self, session: &str) {
        if let Some(lifecycle) = self.lifecycle(session) {
            if let Some(binding) = self.history.get_mut(&(session.into(), lifecycle)) {
                binding.registered = false;
            }
        }
    }
}
