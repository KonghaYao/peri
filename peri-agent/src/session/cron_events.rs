//! MCP-only cron event consumption shared by session and one-shot clients.
use peri_acp_types::{cron::CronTrigger, ports::McpPoolPort};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The server retains each firing until delivery succeeds and tools/call acknowledges it.
/// Retry failed acknowledgements without duplicating an already accepted local delivery.
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
