use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::agent::async_tasks::registry::BackgroundTaskRegistry;

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

/// 第 `attempt` 次重试（0 起）前的退避。worker 与定时测试共用同一调度，
/// 避免测试预算写死成与调度脱节的步数。
pub(super) fn retry_delay(attempt: u32) -> Duration {
    INITIAL_RETRY_DELAY
        .saturating_mul(1_u32 << attempt.min(16))
        .min(MAX_RETRY_DELAY)
}

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
            let mut attempt = 0_u32;
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = peri_time::sleep(retry_delay(attempt)) => {}
                }
                let Some(registry) = owner.upgrade() else {
                    break;
                };
                if cancel.is_cancelled() {
                    break;
                }
                // 重试复用生产者的同步投递桥；等待存储提交同样不能占住 worker。
                let retry_registry = Arc::clone(&registry);
                if let Err(error) =
                    tokio::task::spawn_blocking(move || retry_registry.retry_pending_deliveries())
                        .await
                {
                    tracing::warn!(%error, "pending task delivery retry task failed");
                }
                let mut running = registry.delivery_worker_running.lock();
                if registry.pending_deliveries.lock().is_empty()
                    && registry.settlements_in_flight.load(Ordering::SeqCst) == 0
                {
                    *running = false;
                    return;
                }
                drop(running);
                drop(registry);
                attempt = attempt.saturating_add(1);
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
