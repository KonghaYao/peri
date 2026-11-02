use super::{BackgroundRegistryError, BackgroundTaskRegistry};
use crate::agent::events::BackgroundTaskResult;
use peri_acp_types::tasks::{BgRegistryEvent, BgTaskKind, OnBgCompleteFn, TaskRegistrationTicket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[path = "delivery_retry.rs"]
mod delivery_retry;

struct SettlementGuard<'owner>(&'owner AtomicUsize);

impl<'owner> SettlementGuard<'owner> {
    fn new(in_flight: &'owner AtomicUsize) -> Self {
        in_flight.fetch_add(1, Ordering::SeqCst);
        Self(in_flight)
    }
}

impl Drop for SettlementGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) struct PendingTaskDelivery {
    ticket: TaskRegistrationTicket,
    result: BackgroundTaskResult,
    kind: BgTaskKind,
    delivery: OnBgCompleteFn,
    commit_terminal: bool,
}

impl BackgroundTaskRegistry {
    pub fn settle_completed(
        self: &Arc<Self>,
        ticket: &TaskRegistrationTicket,
        mut result: BackgroundTaskResult,
        delivery: OnBgCompleteFn,
    ) -> Result<bool, BackgroundRegistryError> {
        let _settlement = SettlementGuard::new(&self.settlements_in_flight);
        self.validate_ticket(ticket)?;
        if ticket.is_settled() {
            return Ok(false);
        }
        let task_id = ticket.task_id();
        let kind = self.tasks.lock().get(task_id).map(|task| task.kind);
        let commit_terminal = self.claim_completion(ticket);
        let kind = if commit_terminal {
            kind.ok_or_else(|| BackgroundRegistryError::TaskNotFound(task_id.into()))?
        } else if let Some(kind) = self.claim_cancelled_cleanup(ticket) {
            result.success = false;
            if kind == BgTaskKind::Shell {
                result.output = "Shell command cancelled; read the output files as needed.".into();
            }
            kind
        } else {
            return if ticket.is_settled() {
                Ok(false)
            } else {
                Err(BackgroundRegistryError::TaskCompleting(task_id.into()))
            };
        };
        result.task_id = task_id.to_owned();
        let outcome = self.deliver_pending(PendingTaskDelivery {
            ticket: ticket.clone(),
            result,
            kind,
            delivery,
            commit_terminal,
        });
        if outcome.is_err() {
            self.start_delivery_worker();
        }
        outcome
    }

    pub fn retry_pending_deliveries(&self) -> usize {
        let _settlement = SettlementGuard::new(&self.settlements_in_flight);
        let pending: Vec<_> = self
            .pending_deliveries
            .lock()
            .drain()
            .map(|(_, value)| value)
            .collect();
        pending
            .into_iter()
            .map(|delivery| match self.deliver_pending(delivery) {
                Ok(_) => true,
                Err(error) => {
                    tracing::warn!(%error, "task completion delivery remains pending");
                    false
                }
            })
            .filter(|delivered| *delivered)
            .count()
    }

    fn deliver_pending(
        &self,
        pending: PendingTaskDelivery,
    ) -> Result<bool, BackgroundRegistryError> {
        let task_id = pending.result.task_id.clone();
        let delivered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (pending.delivery)(&pending.result, pending.kind)
        }))
        .unwrap_or_else(|_| Err("completion delivery panicked".into()));
        if let Err(reason) = delivered {
            if pending.commit_terminal {
                let mut projection = self.projection.lock();
                if let Some(record) = projection.records.get_mut(&task_id) {
                    record.status = "delivery_pending".into();
                    record.duration_ms = pending.result.duration_ms;
                }
                self.push_event(
                    &mut projection,
                    BgRegistryEvent::Updated {
                        task_id: task_id.clone(),
                        status: "delivery_pending".into(),
                    },
                );
            }
            self.pending_deliveries
                .lock()
                .insert(task_id.clone(), pending);
            self.notify_activity_change();
            return Err(BackgroundRegistryError::DeliveryFailed { task_id, reason });
        }
        if pending.commit_terminal {
            if !self.complete(&pending.ticket, pending.result) {
                return Err(BackgroundRegistryError::TaskNotFound(task_id));
            }
            Ok(true)
        } else {
            self.mark_registration_settled(&pending.ticket);
            self.retire_settled_projection(&pending.ticket);
            self.notify_activity_change();
            Ok(false)
        }
    }
}

#[cfg(test)]
#[path = "settlement_test.rs"]
mod tests;
