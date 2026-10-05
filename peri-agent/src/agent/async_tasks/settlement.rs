use super::{BackgroundRegistryError, BackgroundTaskRegistry};
use crate::agent::events::BackgroundTaskResult;
use peri_acp_types::tasks::{BgRegistryEvent, BgTaskKind, OnBgCompleteFn};
use std::sync::atomic::{AtomicUsize, Ordering};

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
    result: BackgroundTaskResult,
    kind: BgTaskKind,
    delivery: OnBgCompleteFn,
    commit_terminal: bool,
}

impl BackgroundTaskRegistry {
    pub fn settle_completed(
        &self,
        task_id: &str,
        mut result: BackgroundTaskResult,
        delivery: OnBgCompleteFn,
    ) -> Result<bool, BackgroundRegistryError> {
        let _settlement = SettlementGuard::new(&self.settlements_in_flight);
        let kind = self.tasks.lock().get(task_id).map(|task| task.kind);
        let commit_terminal = self.claim_completion(task_id);
        let kind = if commit_terminal {
            kind.ok_or_else(|| BackgroundRegistryError::TaskNotFound(task_id.into()))?
        } else if self.claim_cancelled_shell_cleanup(task_id) {
            result.success = false;
            result.output = "Shell command cancelled; read the output files as needed.".into();
            BgTaskKind::Shell
        } else {
            return match self.projection_status(task_id).as_deref() {
                Some("completed" | "failed" | "cancelled") => Ok(false),
                Some(_) => Err(BackgroundRegistryError::TaskCompleting(task_id.into())),
                None => Err(BackgroundRegistryError::TaskNotFound(task_id.into())),
            };
        };
        result.task_id = task_id.to_owned();
        self.deliver_pending(PendingTaskDelivery {
            result,
            kind,
            delivery,
            commit_terminal,
        })
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
            if !self.complete(&task_id, pending.result) {
                return Err(BackgroundRegistryError::TaskNotFound(task_id));
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
#[path = "settlement_test.rs"]
mod tests;
