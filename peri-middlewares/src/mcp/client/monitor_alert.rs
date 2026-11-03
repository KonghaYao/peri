//! Monitor alert delivery policy for the host task-subscription poller.
//!
//! The builtin monitor instance projects a cumulative alert hit counter in
//! `tasks/get` metadata. This module owns the host side of the contract:
//!
//! - parse the projection from the result's `_meta`;
//! - decide whether the current observation should deliver, drop offline, or
//!   stay suppressed (30-second merge window);
//! - push a canonical `Defer` reminder into the initiating session inbox.
//!
//! The decision function is pure (time is injected) so the policy can be unit
//! tested without a running MCP instance. The subscription loop only fetches
//! metadata, executes the action, and keeps the per-task state.

use std::time::{Duration, Instant};

use peri_acp_types::monitor::{
    monitor_alert_delivery_id, monitor_alert_reminder, MonitorAlertMeta, MONITOR_META_KEY,
};
use peri_acp_types::session::{MessageKind, MessageSource};
use rmcp::model::MetaObject;

use super::McpClientPool;

/// Minimum interval between two non-terminal alert reminders for one task.
pub(crate) const MONITOR_ALERT_MERGE_WINDOW: Duration = Duration::from_secs(30);

/// Per-task alert delivery state. It lives in the subscription loop, which is
/// serial for one task, so no lock is needed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MonitorAlertState {
    /// Cumulative hit count already delivered or explicitly dropped.
    processed: u64,
    /// Earliest time the next non-terminal alert may be delivered.
    next_allowed: Option<Instant>,
}

/// Action chosen from one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MonitorAlertAction {
    /// No unprocessed hits.
    Nothing,
    /// Deliver a reminder covering the cumulative hit count.
    Send { hits: u64 },
    /// The initiating session has no inbox; drop without delivery.
    DropOffline { hits: u64 },
    /// Inside the merge window; keep the count for the next observation.
    Suppressed,
}

impl MonitorAlertState {
    #[cfg(test)]
    pub(crate) fn processed(&self) -> u64 {
        self.processed
    }

    #[cfg(test)]
    pub(crate) fn next_allowed(&self) -> Option<Instant> {
        self.next_allowed
    }

    /// Choose the action for one observation.
    ///
    /// `hits` is the server-side cumulative count. A zero initial `processed`
    /// means the first observation with `hits > 0` delivers immediately,
    /// including matches that happened before the first poll: there is no
    /// baseline snapshot. Terminal observations bypass the merge window so
    /// unreported hits are flushed before settlement; they do not create a
    /// second reminder when the count is unchanged.
    pub(crate) fn decide(
        &mut self,
        hits: u64,
        now: Instant,
        terminal: bool,
        has_inbox: bool,
    ) -> MonitorAlertAction {
        if hits <= self.processed {
            return MonitorAlertAction::Nothing;
        }
        if !has_inbox {
            self.processed = hits;
            return MonitorAlertAction::DropOffline { hits };
        }
        if terminal || self.next_allowed.is_none_or(|next| now >= next) {
            self.processed = hits;
            self.next_allowed = Some(now + MONITOR_ALERT_MERGE_WINDOW);
            return MonitorAlertAction::Send { hits };
        }
        MonitorAlertAction::Suppressed
    }
}

/// Parse the monitor projection from a `tasks/get` result `_meta`.
///
/// Missing metadata or a malformed field is an error: the caller must not treat
/// it as zero hits, because that would silently skip a real alert.
pub(crate) fn parse_monitor_meta(
    meta: Option<&MetaObject>,
) -> Result<MonitorAlertMeta, &'static str> {
    let value = meta
        .and_then(|meta| meta.0.get(MONITOR_META_KEY))
        .ok_or("tasks/get response is missing _meta[\"peri/monitor\"]")?;
    serde_json::from_value(value.clone())
        .map_err(|_| "tasks/get _meta[\"peri/monitor\"] has an invalid field type")
}

/// Push the alert reminder for `hits` into the initiating session inbox.
///
/// `processed` has already advanced before this call (the decision is final),
/// so a missing inbox race or a rejected reminder is a deterministic drop, not
/// a retry. Returns `false` when nothing was pushed.
pub(crate) async fn push_monitor_alert(
    pool: &McpClientPool,
    session_id: &str,
    raw_monitor_id: &str,
    meta: &MonitorAlertMeta,
    hits: u64,
) -> bool {
    let inbox = {
        let bindings = pool.session_bindings.read();
        bindings.inbox(session_id)
    };
    let Some(inbox) = inbox else {
        tracing::warn!(
            session_id = %session_id,
            task_id = %raw_monitor_id,
            hits,
            "monitor alert delivery skipped: initiating session inbox is gone"
        );
        return false;
    };
    let reminder = match monitor_alert_reminder(
        raw_monitor_id,
        meta.label.as_deref(),
        hits,
        meta.last_alert_seq,
    ) {
        Ok(reminder) => reminder,
        Err(error) => {
            tracing::warn!(
                session_id = %session_id,
                task_id = %raw_monitor_id,
                hits,
                %error,
                "monitor alert reminder rejected"
            );
            return false;
        }
    };
    let outcome = inbox
        .push_system_reminder_with_delivery_id(
            MessageKind::Defer,
            MessageSource::McpNotification,
            reminder,
            monitor_alert_delivery_id(raw_monitor_id, hits),
        )
        .await;
    if let Err(error) = outcome {
        tracing::warn!(
            session_id = %session_id,
            task_id = %raw_monitor_id,
            hits,
            %error,
            "monitor alert reminder enqueue failed"
        );
        return false;
    }
    tracing::debug!(
        session_id = %session_id,
        task_id = %raw_monitor_id,
        hits,
        "monitor alert reminder delivered"
    );
    true
}

#[cfg(test)]
#[path = "monitor_alert_test.rs"]
mod tests;
