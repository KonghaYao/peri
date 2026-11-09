use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::session::{InboxHandle, MessageQueue};
use peri_acp_types::tasks::TaskManager;

/// Who owns a session binding.
///
/// Initial sessions are registered/unregistered by the session manager; child
/// agent sessions are bound for the duration of the child and released when its
/// resources settle. The kind is bookkeeping only — liveness is still decided
/// by the `registered` flag and the inbox handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingKind {
    Initial,
    Agent,
}

#[derive(Default)]
struct Binding {
    kind: Option<BindingKind>,
    inbox: Option<InboxHandle>,
    /// 子会话的同一 mailbox；resume owner 复用，生产者不从此处取用。
    queue: Option<MessageQueue>,
    manager: Option<Arc<dyn TaskManager>>,
    registered: bool,
}

#[derive(Default)]
pub(crate) struct SessionBindings {
    current: HashMap<String, Binding>,
}

impl SessionBindings {
    /// Owners with a live (registered) inbox.
    #[cfg(test)]
    pub(crate) fn registered_owners(&self) -> Vec<String> {
        self.current
            .keys()
            .filter(|session| self.inbox(session).is_some())
            .cloned()
            .collect()
    }

    /// Binding kind for observability and tests.
    #[cfg(test)]
    pub(crate) fn kind(&self, session: &str) -> Option<BindingKind> {
        self.current.get(session)?.kind
    }

    /// 收集需要关闭检查的子会话句柄；锁不跨 await，`has_required` 由调用方在锁外查询。
    pub(crate) fn close_candidates(
        &self,
        root: &str,
    ) -> Result<Vec<(String, Arc<dyn TaskManager>, InboxHandle)>, String> {
        let mut candidates = Vec::new();
        for (session, binding) in &self.current {
            if session == root {
                continue;
            }
            let manager = binding
                .manager
                .as_ref()
                .ok_or_else(|| format!("Incomplete: child {session} ownership unknown"))?
                .clone();
            let inbox = binding
                .inbox
                .as_ref()
                .ok_or_else(|| format!("Incomplete: child {session} inbox unknown"))?;
            candidates.push((session.clone(), manager, inbox.clone()));
        }
        Ok(candidates)
    }

    pub(crate) fn binding(
        &self,
        session: &str,
    ) -> Option<(InboxHandle, MessageQueue, Arc<dyn TaskManager>)> {
        let binding = self.current.get(session)?;
        Some((
            binding.inbox.clone()?,
            binding.queue.clone()?,
            binding.manager.clone()?,
        ))
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
        let binding = self.current.entry(session.into()).or_default();
        binding.kind.get_or_insert(BindingKind::Initial);
        binding.manager = Some(manager);
    }

    pub(crate) fn register_initial_inbox(&mut self, session: &str, inbox: InboxHandle) {
        let binding = self.current.entry(session.into()).or_default();
        binding.kind = Some(BindingKind::Initial);
        binding.inbox = Some(inbox);
        binding.registered = true;
    }

    /// Bind a child agent session for its whole lifetime.
    pub(crate) fn bind_agent(
        &mut self,
        session: &str,
        inbox: InboxHandle,
        queue: MessageQueue,
        manager: Arc<dyn TaskManager>,
    ) {
        let binding = self.current.entry(session.into()).or_default();
        binding.kind = Some(BindingKind::Agent);
        binding.inbox = Some(inbox);
        binding.queue = Some(queue);
        binding.manager = Some(manager);
        binding.registered = true;
    }

    pub(crate) fn unregister(&mut self, session: &str) {
        if let Some(binding) = self.current.get_mut(session) {
            binding.registered = false;
        }
    }

    /// 终态停驻：只翻 Agent binding 的注册位，保留 owner／queue 供 resume 复用。
    ///
    /// 根会话与其他 binding 不参与；resume 装配经 [`Self::bind_agent`] 重新注册。
    pub(crate) fn park_agent(&mut self, session: &str) {
        if let Some(binding) = self.current.get_mut(session) {
            if binding.kind == Some(BindingKind::Agent) {
                binding.registered = false;
            }
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
