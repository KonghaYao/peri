//! MCP-only cron event consumption shared by session and one-shot clients.
use peri_acp_types::cron::CronTrigger;
use peri_acp_types::ports::McpPoolPort;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The server retains each firing until delivery succeeds and tools/call acknowledges it.
/// Retry failed acknowledgements without duplicating an already accepted local delivery.
/// Dropping the returned handle detaches the task; `shutdown` controls its lifetime.
pub fn forward_cron_triggers(
    pool: Arc<dyn McpPoolPort>,
    shutdown: CancellationToken,
    mut deliver: impl FnMut(CronTrigger) -> bool + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = peri_time::interval(std::time::Duration::from_millis(250));
        interval.set_missed_tick_behavior(peri_time::MissedTickBehavior::Delay);
        // Sending to continuation and acknowledging the MCP owner are separate
        // operations. Retry a failed acknowledgement without resending locally.
        let mut delivered = std::collections::HashSet::new();
        let mut retry_delay = std::time::Duration::from_millis(250);
        let mut unavailable = false;
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {},
            }
            let read = tokio::select! {
                _ = shutdown.cancelled() => break,
                read = pool.read_instance_resource("cron", "cron://triggers") => read,
            };
            let page = match read.and_then(|text| {
                serde_json::from_str::<TriggerPage>(&text).map_err(|error| error.to_string())
            }) {
                Ok(page) => {
                    unavailable = false;
                    retry_delay = std::time::Duration::from_millis(250);
                    page
                }
                Err(error) => {
                    if is_permanent_cron_unavailable(&error) {
                        tracing::debug!(%error, "cron MCP unavailable; stopping trigger polling");
                        break;
                    }
                    if !unavailable {
                        tracing::warn!(%error, "cron MCP trigger read failed; retaining pending delivery");
                    }
                    unavailable = true;
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = peri_time::sleep(retry_delay) => {},
                    }
                    retry_delay = (retry_delay * 2).min(std::time::Duration::from_secs(5));
                    continue;
                }
            };
            delivered.retain(|key| {
                page.triggers
                    .iter()
                    .any(|trigger| key == &(trigger.task_id.clone(), trigger.firing_id.clone()))
            });
            for trigger in page.triggers {
                let key = (trigger.task_id.clone(), trigger.firing_id.clone());
                if !delivered.contains(&key) {
                    if !deliver(trigger.clone()) {
                        return;
                    }
                    delivered.insert(key);
                }
                let acknowledgement = tokio::select! {
                    _ = shutdown.cancelled() => return,
                    result = pool.call_instance_tool("cron", "cron_acknowledge", serde_json::json!({
                        "task_id": trigger.task_id, "firing_id": trigger.firing_id,
                    }), None) => result,
                };
                if let Err(error) = acknowledgement {
                    tracing::warn!(%error, "cron MCP trigger acknowledgement failed; retrying");
                }
            }
        }
    })
}
#[derive(serde::Deserialize)]
struct TriggerPage {
    triggers: Vec<CronTrigger>,
}

fn is_permanent_cron_unavailable(error: &str) -> bool {
    error.contains("capability `cron` is disabled")
        || error.contains("MCP pool is closing")
        || error.contains("MCP connection is stopping")
        || error.contains("is disabled")
        || error.contains("not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::ports::McpPoolShutdownReport;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DisabledCronPool {
        reads: AtomicUsize,
        error_message: &'static str,
    }

    #[async_trait::async_trait]
    impl McpPoolPort for DisabledCronPool {
        async fn shutdown(&self) -> McpPoolShutdownReport {
            McpPoolShutdownReport::Complete {
                settled_services: 0,
                failed_services: 0,
            }
        }

        fn snapshot(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn read_instance_resource(
            &self,
            _instance: &str,
            _uri: &str,
        ) -> Result<String, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Err(self.error_message.to_owned())
        }
    }

    #[tokio::test]
    async fn forward_cron_triggers_stops_immediately_on_permanent_unavailability() {
        for error_message in ["capability `cron` is disabled", "MCP pool is closing"] {
            let pool = Arc::new(DisabledCronPool {
                reads: AtomicUsize::new(0),
                error_message,
            });
            let cancel = CancellationToken::new();
            let handle = forward_cron_triggers(pool.clone(), cancel.clone(), |_| true);
            tokio::time::timeout(std::time::Duration::from_secs(2), handle)
                .await
                .expect("cron polling must exit instead of sleeping 5s in a loop")
                .expect("task should complete cleanly");
            assert_eq!(pool.reads.load(Ordering::SeqCst), 1);
        }
    }
}
