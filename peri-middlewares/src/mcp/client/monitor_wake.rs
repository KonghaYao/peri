//! Host-side monitor wake pump.
//!
//! The builtin `monitor` instance holds alert matches in a per-task outbox until
//! the host acknowledges them. This pump polls `monitor_wakes` per known owner,
//! delivers a `Defer` reminder into that owner's inbox, and acknowledges the
//! wake.
//!
//! Boundaries:
//! - **owner-directed**: one `monitor_wakes` call per owner with
//!   `_meta["peri/taskOwner"]`; a wake is never broadcast.
//! - **no approval on the wake path**: `monitor_start`'s tool approval covers
//!   later wakes. This pump never calls HITL or the cron continuation path.
//! - **no event text**: the reminder carries only task id, label, seq, and the
//!   matched count; output is pulled with `monitor_read` as untrusted data.
//! - **offline owners drop**: no inbox ⇒ no push; the wake is acknowledged as
//!   `offline` and counted. Wakes are never retargeted to another session.
//! - **idempotent**: the delivery id is derived from `(task_id, wake_id)`; a
//!   retry after a failed acknowledgement reuses the same canonical id.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};

use peri_acp_types::monitor::{
    monitor_wake_delivery_id, monitor_wake_reminder, MonitorAckOutcome, MonitorWake,
    MonitorWakePage,
};
use peri_acp_types::session::{MessageKind, MessageSource};

use super::McpClientPool;
use crate::mcp::task_scope::McpTaskKey;

const MONITOR_INSTANCE: &str = "monitor";
const MONITOR_WAKE_TOOL: &str = "monitor_wakes";
const MONITOR_ACK_TOOL: &str = "monitor_ack";
const MONITOR_WAKE_POLL_INTERVAL: Duration = Duration::from_millis(750);
const MONITOR_WAKE_LIMIT: usize = 64;
/// How long a released owner stays tracked after its binding disappears.
/// The window lets an in-flight wake be acknowledged as `offline`; afterwards
/// the owner is dropped so the pump set stays bounded.
const MONITOR_OFFLINE_OWNER_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct MonitorWakeMetrics {
    pub(crate) delivered: AtomicU64,
    pub(crate) offline: AtomicU64,
    pub(crate) ack_failures: AtomicU64,
    pub(crate) poll_failures: AtomicU64,
}

/// Pool-owned wake pump state.
#[derive(Default)]
pub(crate) struct MonitorWakeRuntime {
    started: AtomicBool,
    /// Owners that ever started a monitor task in this pool. Owners without a
    /// monitor task are never added. A released binding is dropped after
    /// [`MONITOR_OFFLINE_OWNER_GRACE`], so the set cannot grow without bound.
    owners: parking_lot::Mutex<HashSet<String>>,
    /// First observation time of each currently offline tracked owner.
    offline_since: parking_lot::Mutex<HashMap<String, Instant>>,
    pub(crate) metrics: MonitorWakeMetrics,
}

impl McpClientPool {
    /// Register an owner and ensure the wake pump is running.
    ///
    /// Called when the first monitor task is admitted for a session. Repeated
    /// calls only add owners; the pump itself starts at most once per pool.
    pub(crate) fn ensure_monitor_wake_pump(self: &Arc<Self>, owner: &str) {
        if !self.is_open() {
            return;
        }
        if self.get_client(MONITOR_INSTANCE).is_none() {
            tracing::debug!(owner = %owner, "monitor wake pump skipped: monitor instance absent");
            return;
        }
        self.monitor_wake.owners.lock().insert(owner.to_owned());
        self.monitor_wake.offline_since.lock().remove(owner);
        if self.monitor_wake.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(self);
        if let Err(error) = self.spawn_background(McpTaskKey::MonitorWake, run_pump(weak)) {
            self.monitor_wake.started.store(false, Ordering::Release);
            tracing::warn!(%error, "monitor wake pump was not admitted");
        }
    }

    /// Whether an owner currently has a live inbox.
    pub(crate) fn owner_online(&self, owner: &str) -> bool {
        self.session_bindings.read().inbox(owner).is_some()
    }

    /// Whether a connected server is the builtin monitor instance.
    ///
    /// The authoritative identity is the runtime-owned
    /// `ConfigSource::Builtin { instance }`, which user configuration cannot
    /// forge. The reserved name is only a fallback for directly constructed
    /// pools (tests / local wiring): a non-builtin connection named `monitor`
    /// would also match it and have its terminal reminders suppressed. Normal
    /// configuration cannot reach that state — `monitor` is a reserved builtin
    /// instance name, and `apply_builtin_overlay` rejects any `command`/`url`
    /// takeover at load time, even when `PERI_MCP_BUILTIN=off`.
    pub(crate) fn is_monitor_server(&self, server: &str) -> bool {
        if server == MONITOR_INSTANCE {
            return true;
        }
        self.clients.read().get(server).is_some_and(|client| {
            matches!(
                client.source.as_ref(),
                Some(crate::mcp::config::ConfigSource::Builtin { instance })
                    if instance == MONITOR_INSTANCE
            )
        })
    }

    /// One pump pass. Returns `false` when the monitor instance is absent or not
    /// connected (the pump then stays idle rather than treating it as an error).
    pub(crate) async fn poll_monitor_wakes_once(&self) -> bool {
        let Some(handle) = self.get_client(MONITOR_INSTANCE) else {
            return false;
        };
        if !matches!(handle.status, super::ClientStatus::Connected) || handle.peer.is_none() {
            return false;
        }
        // Only owners that actually started a monitor task are polled. Merging
        // every registered session would grow the set with sessions that never
        // used monitor and would issue one owner-scoped call per session per tick.
        let owners: Vec<String> = self.monitor_wake.owners.lock().iter().cloned().collect();
        for owner in owners {
            if !self.is_open() {
                break;
            }
            let page = match self.fetch_wakes(&owner).await {
                Ok(page) => Some(page),
                Err(error) => {
                    let previous = self
                        .monitor_wake
                        .metrics
                        .poll_failures
                        .fetch_add(1, Ordering::Relaxed);
                    if previous == 0 {
                        tracing::warn!(owner = %owner, %error, "monitor wake poll failed; retrying");
                    } else {
                        tracing::debug!(owner = %owner, %error, "monitor wake poll still failing");
                    }
                    None
                }
            };
            if let Some(page) = page {
                if !page.wakes.is_empty() {
                    self.deliver_owner_wakes(&owner, page.wakes).await;
                }
            }
            // Binding release (child session end / initial session teardown)
            // removes the delivery target. Keep the owner for a short grace so
            // an in-flight wake can still be acknowledged offline, then drop
            // it from the pump set. Wakes produced afterwards stay
            // unacknowledged in the server outbox; they are observable through
            // `monitor_list`/`tasks/get` counters but are never retargeted to
            // another session.
            self.retire_offline_owner(&owner);
        }
        true
    }

    /// Track owner liveness and drop released owners after the grace window.
    fn retire_offline_owner(&self, owner: &str) {
        if self.owner_online(owner) {
            self.monitor_wake.offline_since.lock().remove(owner);
            return;
        }
        let mut offline_since = self.monitor_wake.offline_since.lock();
        match offline_since.get(owner).copied() {
            None => {
                offline_since.insert(owner.to_owned(), Instant::now());
            }
            Some(since) if since.elapsed() >= MONITOR_OFFLINE_OWNER_GRACE => {
                offline_since.remove(owner);
                drop(offline_since);
                self.monitor_wake.owners.lock().remove(owner);
            }
            Some(_) => {}
        }
    }

    async fn fetch_wakes(&self, owner: &str) -> Result<MonitorWakePage, String> {
        let text = self
            .call_instance_tool(
                MONITOR_INSTANCE,
                MONITOR_WAKE_TOOL,
                serde_json::json!({ "limit": MONITOR_WAKE_LIMIT }),
                Some(owner_meta(owner)),
            )
            .await?;
        serde_json::from_str(&text).map_err(|error| format!("monitor_wakes response: {error}"))
    }

    async fn deliver_owner_wakes(&self, owner: &str, wakes: Vec<MonitorWake>) {
        let inbox = self
            .owner_online(owner)
            .then(|| self.session_bindings.read().inbox(owner))
            .flatten();
        let Some(inbox) = inbox else {
            // Offline: drop without delivery, but clear the server outbox so the
            // task's pending count reflects that nobody will read it.
            let ids: Vec<String> = wakes.iter().map(|wake| wake.wake_id.clone()).collect();
            match self
                .ack_wakes(owner, &ids, MonitorAckOutcome::Offline)
                .await
            {
                Ok(acked) => {
                    self.monitor_wake
                        .metrics
                        .offline
                        .fetch_add(acked, Ordering::Relaxed);
                }
                Err(error) => self.note_ack_failure(owner, &error),
            }
            return;
        };
        for wake in wakes {
            let delivery_id = monitor_wake_delivery_id(&wake.task_id, &wake.wake_id);
            let reminder = match monitor_wake_reminder(&wake) {
                Ok(reminder) => reminder,
                Err(error) => {
                    tracing::warn!(
                        owner = %owner,
                        wake_id = %wake.wake_id,
                        %error,
                        "monitor wake reminder rejected"
                    );
                    continue;
                }
            };
            inbox.push_system_reminder_with_delivery_id(
                MessageKind::Defer,
                MessageSource::McpNotification,
                reminder,
                delivery_id,
            );
            self.monitor_wake
                .metrics
                .delivered
                .fetch_add(1, Ordering::Relaxed);
            match self
                .ack_wakes(
                    owner,
                    std::slice::from_ref(&wake.wake_id),
                    MonitorAckOutcome::Delivered,
                )
                .await
            {
                Ok(_) => {}
                Err(error) => self.note_ack_failure(owner, &error),
            }
        }
    }

    async fn ack_wakes(
        &self,
        owner: &str,
        wake_ids: &[String],
        outcome: MonitorAckOutcome,
    ) -> Result<u64, String> {
        let text = self
            .call_instance_tool(
                MONITOR_INSTANCE,
                MONITOR_ACK_TOOL,
                serde_json::json!({
                    "wake_ids": wake_ids,
                    "outcome": outcome.as_str(),
                }),
                Some(owner_meta(owner)),
            )
            .await?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("monitor_ack response: {error}"))?;
        value
            .get("acked")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "monitor_ack response is missing 'acked'".to_owned())
    }

    fn note_ack_failure(&self, owner: &str, error: &str) {
        let previous = self
            .monitor_wake
            .metrics
            .ack_failures
            .fetch_add(1, Ordering::Relaxed);
        if previous == 0 {
            tracing::warn!(owner = %owner, %error, "monitor wake acknowledgement failed; retrying");
        } else {
            tracing::debug!(owner = %owner, %error, "monitor wake acknowledgement still failing");
        }
    }
}

fn owner_meta(owner: &str) -> serde_json::Value {
    serde_json::json!({ peri_mcp_core::task_owner::TASK_OWNER_META_KEY: owner })
}

/// Pump loop. Stops when the pool closes or the pool is dropped.
async fn run_pump(pool: Weak<McpClientPool>) {
    loop {
        let Some(pool) = pool.upgrade() else {
            return;
        };
        if !pool.is_open() {
            return;
        }
        let _ = pool.poll_monitor_wakes_once().await;
        drop(pool);
        peri_time::sleep(MONITOR_WAKE_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
#[path = "monitor_wake_test.rs"]
mod tests;
