use std::collections::HashMap;

use peri_acp_types::mcp_apps::AppSessionBinding;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectionLifecycle {
    Open,
    Closing,
    Closed,
}

pub(crate) struct ConnectionContext {
    id: String,
    lifecycle: ConnectionLifecycle,
    initialized: bool,
    apps_enabled: bool,
    app_sessions: HashMap<String, AppSessionBinding>,
    cancellation: tokio_util::sync::CancellationToken,
}

impl ConnectionContext {
    pub(crate) fn new(apps_enabled: bool) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            lifecycle: ConnectionLifecycle::Open,
            initialized: false,
            apps_enabled,
            app_sessions: HashMap::new(),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub(crate) fn commit_initialize(&mut self) {
        if !self.initialized && self.lifecycle == ConnectionLifecycle::Open {
            self.initialized = true;
        }
    }

    pub(crate) fn apps_enabled(&self) -> bool {
        self.initialized && self.lifecycle == ConnectionLifecycle::Open && self.apps_enabled
    }

    #[allow(dead_code)]
    pub(crate) fn insert_app_session(&mut self, binding: AppSessionBinding) -> bool {
        if self.apps_enabled() && binding.owner_connection_id == self.id {
            self.app_sessions
                .insert(binding.app_session_id.clone(), binding);
            true
        } else {
            false
        }
    }

    pub(crate) fn app_session(&self, id: &str) -> Option<&AppSessionBinding> {
        self.apps_enabled()
            .then(|| self.app_sessions.get(id))
            .flatten()
    }

    pub(crate) fn snapshot_for_request(&self) -> Self {
        Self {
            id: self.id.clone(),
            lifecycle: self.lifecycle,
            initialized: self.initialized,
            apps_enabled: self.apps_enabled,
            app_sessions: self.app_sessions.clone(),
            cancellation: self.cancellation.clone(),
        }
    }

    pub(crate) fn cancellation(&self) -> tokio_util::sync::CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) fn begin_close(&mut self) {
        self.lifecycle = ConnectionLifecycle::Closing;
        self.cancellation.cancel();
        self.app_sessions.clear();
    }

    #[allow(dead_code)]
    pub(crate) fn finish_close(&mut self) {
        self.lifecycle = ConnectionLifecycle::Closed;
        self.app_sessions.clear();
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
#[path = "connection_test.rs"]
mod tests;
