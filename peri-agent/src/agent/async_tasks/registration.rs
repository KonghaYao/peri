use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use peri_acp_types::tasks::{BgTaskKind, TaskRegistrationReceipt, TaskRegistrationTicket};

use super::{BackgroundRegistryError, BackgroundTaskRegistry};

pub(super) struct RegistrationReceipt {
    task_id: String,
    owner_id: uuid::Uuid,
    source_identity: Option<String>,
    pub(super) initiator_session_id: parking_lot::RwLock<Option<String>>,
    settled: AtomicBool,
    started: AtomicBool,
    pub(super) kind: BgTaskKind,
    pub(super) owner_session_id: Option<String>,
    pub(super) owner_identity: Option<String>,
}

impl RegistrationReceipt {
    pub(super) fn new(
        task_id: String,
        owner_id: uuid::Uuid,
        source_identity: Option<String>,
        task: &super::BackgroundTask,
        started: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            task_id,
            owner_id,
            source_identity,
            initiator_session_id: parking_lot::RwLock::new(task.initiator_session_id.clone()),
            settled: AtomicBool::new(false),
            started: AtomicBool::new(started),
            kind: task.kind,
            owner_session_id: task.owner_session_id.clone(),
            owner_identity: task.owner_identity.clone(),
        })
    }

    pub(super) fn ticket(self: &Arc<Self>) -> TaskRegistrationTicket {
        TaskRegistrationTicket::from_receipt(self.clone())
    }
}

impl TaskRegistrationReceipt for RegistrationReceipt {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn owner_id(&self) -> uuid::Uuid {
        self.owner_id
    }
    fn is_settled(&self) -> bool {
        self.settled.load(Ordering::Acquire)
    }
    fn source_identity(&self) -> Option<&str> {
        self.source_identity.as_deref()
    }
    fn initiator_session_id(&self) -> Option<String> {
        self.initiator_session_id.read().clone()
    }
}

impl BackgroundTaskRegistry {
    pub(in crate::agent::async_tasks) fn mark_execution_started(
        &self,
        ticket: &TaskRegistrationTicket,
    ) -> Result<(), BackgroundRegistryError> {
        let tasks = self.tasks.lock();
        self.validate_ticket(ticket)?;
        if ticket.is_settled()
            || !tasks
                .get(ticket.task_id())
                .is_some_and(|task| matches!(task.status, super::BackgroundTaskStatus::Running))
        {
            return Err(BackgroundRegistryError::TaskNotFound(
                ticket.task_id().into(),
            ));
        }
        let registrations = self.registrations.lock();
        let receipt = registrations
            .get(ticket.task_id())
            .expect("validated registration");
        receipt
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| BackgroundRegistryError::TaskAlreadyStarted(ticket.task_id().into()))?;
        Ok(())
    }

    pub(in crate::agent::async_tasks) fn withdraw_unstarted_registration(
        &self,
        ticket: &TaskRegistrationTicket,
    ) -> Result<bool, BackgroundRegistryError> {
        let mut tasks = self.tasks.lock();
        self.validate_ticket(ticket)?;
        if ticket.is_settled() {
            return Ok(false);
        }
        let receipt = self
            .registrations
            .lock()
            .get(ticket.task_id())
            .cloned()
            .expect("validated registration");
        if receipt.started.load(Ordering::Acquire) {
            return Err(BackgroundRegistryError::TaskAlreadyStarted(
                ticket.task_id().into(),
            ));
        }
        // The closed start gate is the proof that no execution/result exists.
        // Do not abort here: the producer still owns its startup/permit cleanup.
        if let Some(task) = tasks.remove(ticket.task_id()) {
            if let Some(inbox) = task.agent_inbox {
                inbox.close();
            }
            if let Some(cancel) = task.cancel_token {
                cancel.cancel();
            }
        }
        self.cancelled_tasks_waiting_cleanup
            .lock()
            .remove(ticket.task_id());
        self.unsettled_external.lock().remove(ticket.task_id());
        self.mark_registration_settled(ticket);
        let mut projection = self.projection.lock();
        let changed = projection
            .records
            .get_mut(ticket.task_id())
            .is_some_and(|record| {
                let changed = record.status != "cancelled";
                record.status = "cancelled".into();
                changed
            });
        let pruned = self.release_settled_registration(ticket.task_id())
            && projection.retire(ticket.task_id());
        if changed {
            self.push_event(
                &mut projection,
                peri_acp_types::tasks::BgRegistryEvent::Cancelled {
                    task_id: ticket.task_id().into(),
                    reason: "background preparation failed before execution".into(),
                },
            );
        } else if pruned {
            self.push_event(
                &mut projection,
                peri_acp_types::tasks::BgRegistryEvent::Updated {
                    task_id: ticket.task_id().into(),
                    status: "cancelled".into(),
                },
            );
        }
        drop(projection);
        drop(tasks);
        self.notify_activity_change();
        Ok(true)
    }

    pub(in crate::agent::async_tasks) fn retire_settled_projection(
        &self,
        ticket: &TaskRegistrationTicket,
    ) {
        let _tasks = self.tasks.lock();
        let task_id = ticket.task_id();
        let matches = self
            .registrations
            .lock()
            .get(task_id)
            .is_some_and(|receipt| ticket.same_registration(&receipt.ticket()));
        if matches && self.release_settled_registration(task_id) {
            let mut projection = self.projection.lock();
            let status = projection
                .records
                .get(task_id)
                .map(|record| record.status.clone());
            if projection.retire(task_id) {
                if let Some(status) = status {
                    self.push_event(
                        &mut projection,
                        peri_acp_types::tasks::BgRegistryEvent::Updated {
                            task_id: task_id.into(),
                            status,
                        },
                    );
                }
            }
        }
    }

    pub(in crate::agent::async_tasks) fn validate_ticket(
        &self,
        ticket: &TaskRegistrationTicket,
    ) -> Result<(), BackgroundRegistryError> {
        if ticket.owner_id() != self.registration_owner {
            return Err(BackgroundRegistryError::InvalidRegistration(
                ticket.task_id().into(),
            ));
        }
        if ticket.is_settled()
            || self
                .registrations
                .lock()
                .get(ticket.task_id())
                .is_some_and(|receipt| ticket.same_registration(&receipt.ticket()))
        {
            Ok(())
        } else {
            Err(BackgroundRegistryError::InvalidRegistration(
                ticket.task_id().into(),
            ))
        }
    }

    pub(in crate::agent::async_tasks) fn mark_registration_settled(
        &self,
        ticket: &TaskRegistrationTicket,
    ) {
        if let Some(receipt) = self.registrations.lock().get(ticket.task_id()) {
            if ticket.same_registration(&receipt.ticket()) {
                receipt.settled.store(true, Ordering::Release);
            }
        }
    }

    // The producer ticket retains acceptance evidence after removal. There is
    // deliberately no terminal-ID set: pending work is the only registry owner.
    pub(super) fn release_settled_registration(&self, task_id: &str) -> bool {
        let mut registrations = self.registrations.lock();
        if !registrations
            .get(task_id)
            .is_some_and(|receipt| receipt.is_settled())
            || self.unsettled_external.lock().contains(task_id)
            || self
                .cancelled_tasks_waiting_cleanup
                .lock()
                .contains_key(task_id)
            || self.pending_deliveries.lock().contains_key(task_id)
        {
            return false;
        }
        registrations.remove(task_id);
        let target = registrations.len().max(64);
        if registrations.capacity() > target.saturating_mul(2) {
            registrations.shrink_to(target);
        }
        true
    }
}
