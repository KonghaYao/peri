use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use super::BackgroundTaskRegistry;

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

impl BackgroundTaskRegistry {
    pub(super) fn start_delivery_worker(self: &Arc<Self>) {
        let mut running = self.delivery_worker_running.lock();
        if *running || self.pending_deliveries.lock().is_empty() || self.scope.is_closed() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!("pending task delivery requires a runtime or explicit retry");
            return;
        }
        let owner = Arc::downgrade(self);
        let cancel = self.scope.cancel_token();
        match self.scope.spawn(async move {
            let mut delay = INITIAL_RETRY_DELAY;
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = peri_time::sleep(delay) => {}
                }
                let Some(registry) = owner.upgrade() else {
                    break;
                };
                if cancel.is_cancelled() {
                    break;
                }
                registry.retry_pending_deliveries();
                let mut running = registry.delivery_worker_running.lock();
                if registry.pending_deliveries.lock().is_empty()
                    && registry.settlements_in_flight.load(Ordering::SeqCst) == 0
                {
                    *running = false;
                    return;
                }
                drop(running);
                drop(registry);
                delay = delay.saturating_mul(2).min(MAX_RETRY_DELAY);
            }
            Self::finish_delivery_worker(&owner);
        }) {
            Ok(_) => *running = true,
            Err(error) => tracing::warn!(%error, "pending task delivery worker admission failed"),
        }
    }

    fn finish_delivery_worker(owner: &Weak<Self>) {
        if let Some(registry) = owner.upgrade() {
            *registry.delivery_worker_running.lock() = false;
        }
    }
}
