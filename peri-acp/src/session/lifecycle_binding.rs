use std::sync::Arc;

use peri_acp_types::tasks::TaskManager;
use peri_acp_types::thread::ThreadId;

use super::SessionManager;

impl SessionManager {
    pub fn ensure_session_for_lifecycle(
        &self,
        session_id: &str,
        lifecycle: u64,
        cwd: &str,
        task_manager: Arc<dyn TaskManager>,
    ) -> Result<(), String> {
        if lifecycle == 0 {
            return Err("Incomplete: invalid recipient lifecycle".into());
        }
        match self.inner.sessions.entry(session_id.to_owned()) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let current = entry.get();
                if current.recipient_lifecycle > lifecycle {
                    return Err("Incomplete: stale recipient lifecycle".into());
                }
                if current.recipient_lifecycle == lifecycle {
                    if !Arc::ptr_eq(&current.task_manager, &task_manager) {
                        return Err(
                            "Incomplete: lifecycle already has a different task owner".into()
                        );
                    }
                    return Ok(());
                }
                if !current.active_agents.is_empty()
                    || !current.task_manager.is_execution_idle()
                    || current.v2_message_queue.has_required()
                {
                    return Err("Incomplete: previous lifecycle still owns unfinished work".into());
                }
                let closed = current
                    .task_manager
                    .as_any()
                    .downcast_ref::<peri_agent::agent::async_tasks::TaskManager>()
                    .is_some_and(|manager| manager.session_close_settled());
                if !closed {
                    return Err("Incomplete: previous lifecycle resource close is unproven".into());
                }
                if Arc::ptr_eq(&current.task_manager, &task_manager) {
                    return Err("Incomplete: a new lifecycle requires a new task owner".into());
                }
                let mut replacement = self.build_session_with_task_manager(
                    session_id,
                    ThreadId::from(session_id.to_owned()),
                    cwd,
                    Some(task_manager),
                );
                replacement.recipient_lifecycle = lifecycle;
                entry.insert(replacement);
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let mut session = self.build_session_with_task_manager(
                    session_id,
                    ThreadId::from(session_id.to_owned()),
                    cwd,
                    Some(task_manager),
                );
                session.recipient_lifecycle = lifecycle;
                entry.insert(session);
            }
        }
        self.cron_bridge_for(session_id);
        Ok(())
    }
}
