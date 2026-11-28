//! Replay and live-update buffering for the ACP v2 stdio preview.
//!
//! The host marks replayed notifications with `_meta.periReplay`; anything it
//! emits while a replay is streaming without that marker is live activity of an
//! already attached session. The pinned contract requires the agent to hold
//! live updates during replay, so they are parked per session (bounded) and
//! flushed after the resume reply that ends the replay.
//!
//! `state_update` is a snapshot, not an event: foreground transitions still run
//! while holding, but the authoritative post-replay snapshot is emitted before
//! the reply, so parking the older snapshots would only replay a stale view.

use std::collections::VecDeque;

use serde_json::{json, Value};

use crate::transport::types::{AcpError, IncomingMessage, RequestId};

use super::state::{Attach, Connection, Reply, Slot};
use super::{config, errors, lifecycle, updates, V2Stdio, CONFIG};

/// Upper bound on live updates parked during one replay. Past the bound the
/// adapter drops and keeps serving; the client can resynchronize by issuing
/// another `session/resume` (with or without a replay cursor).
pub(super) const MAX_DEFERRED_UPDATES: usize = 1024;

/// Live updates parked while one replay is in flight.
#[derive(Default)]
pub(super) struct Deferred {
    updates: VecDeque<Value>,
    holding: bool,
}

impl Deferred {
    /// A resume with a replay cursor starts holding live updates.
    pub(super) fn begin(&mut self) {
        self.holding = true;
    }

    pub(super) fn is_holding(&self) -> bool {
        self.holding
    }

    /// Park one live update, dropping redundant state snapshots and (past the
    /// bound) the newest arrivals.
    pub(super) fn hold(&mut self, update: Value) {
        if is_state_update(&update) {
            return;
        }
        if self.updates.len() >= MAX_DEFERRED_UPDATES {
            tracing::warn!(
                held = self.updates.len(),
                "ACP v2 replay buffer is full; dropping a live session update"
            );
            return;
        }
        self.updates.push_back(update);
    }

    /// Stop holding and return what was parked, in arrival order.
    pub(super) fn finish(&mut self) -> Vec<Value> {
        self.holding = false;
        self.updates.drain(..).collect()
    }
}

fn is_state_update(update: &Value) -> bool {
    update.get("sessionUpdate").and_then(Value::as_str) == Some("state_update")
}

/// Whether a `session/update` notification belongs to the host's history replay.
///
/// The host stamps the update's `_meta`; the params-level marker is accepted too
/// so a future host shape cannot silently turn replay traffic into live traffic.
pub(super) fn marked(params: &Value, update: &Value) -> bool {
    fn replay_flag(value: &Value) -> bool {
        value
            .get("_meta")
            .and_then(|meta| meta.get("periReplay"))
            .and_then(Value::as_bool)
            == Some(true)
    }
    replay_flag(params) || replay_flag(update)
}

/// The first chunk of each replayed message gets a replacement boundary so a
/// message the client already streamed live is not appended twice. Only
/// replay-marked chunks are tracked; a live chunk must never reset the message.
pub(super) fn reset_replayed_chunk(slot: &mut Slot, update: &Value) -> bool {
    let Some(replay) = slot.replay.as_mut() else {
        return false;
    };
    match (
        update.get("sessionUpdate").and_then(Value::as_str),
        update.get("messageId").and_then(Value::as_str),
    ) {
        (Some(kind), Some(id)) if kind.ends_with("_chunk") => {
            replay.messages.insert((kind.into(), id.into()))
        }
        _ => false,
    }
}

/// Route one live update: write it now, or park it while a replay holds.
pub(super) fn route_live(slot: &mut Slot, update: Value) -> Option<Value> {
    if slot.deferred.is_holding() {
        slot.deferred.hold(update);
        None
    } else {
        Some(update)
    }
}

/// Complete one `session/new` or `session/resume` setup.
///
/// This owns the pinned resume order: the last replayed update, `replay_complete`
/// exactly once for the owning attempt, the current-state snapshot, the reply,
/// and only then the live updates parked during the replay (or the metadata
/// parked while attaching). A stale success answers its own client without
/// ending the takeover's replay or draining what belongs to it.
#[allow(clippy::too_many_arguments)] // the setup reply is one protocol step; each fact is explicit
pub(super) async fn settle_setup(
    stdio: &V2Stdio,
    wire: &mut Connection,
    id: RequestId,
    result: Result<Value, AcpError>,
    session: Option<String>,
    replay: bool,
    created: bool,
    owner: u64,
    two: bool,
) -> Result<(), AcpError> {
    let mut flush: Vec<(String, Value)> = Vec::new();
    let method = if session.is_some() {
        "session/resume"
    } else {
        "session/new"
    };
    let mut sid: Option<String> = session.clone();
    // Live updates taken from the slot when this attempt finishes
    // replay; written after the reply, per the pinned order.
    let mut deferred: Vec<Value> = Vec::new();
    match &result {
        Ok(value) => {
            sid = session
                .as_deref()
                .or_else(|| value.get("sessionId").and_then(Value::as_str))
                .map(str::to_owned);
            if let Some(sid) = sid.as_deref() {
                let slot = wire.insert_slot(sid, Attach::Attached);
                // A `session/new` id is not routable before its
                // reply, so it has no takeover to defer to; a
                // resume completion must still own the slot.
                let owned = session.is_none() || slot.setup == owner;
                slot.attach = Attach::Attached;
                slot.attached = true;
                let foreground = slot.foreground;
                if owned && session.is_some() {
                    deferred = slot.deferred.finish();
                }
                if owned && replay {
                    slot.replay = None;
                    stdio
                        .update(sid, json!({"sessionUpdate":"replay_complete"}))
                        .await?;
                }
                if session.is_some() {
                    // v2 reports the state snapshot and the session
                    // metadata before the resume reply: the snapshot
                    // is required after any replay and before it, and
                    // the host pushes the command table and options
                    // ahead of its own reply.
                    stdio.update(sid, updates::state(foreground, None)).await?;
                    if owned {
                        let held = slot.flush_held();
                        let projected = held
                            .into_iter()
                            .flat_map(|update| {
                                updates::project_update(update, false).unwrap_or_default()
                            })
                            .collect();
                        stdio.emit_now(slot, sid, projected).await?;
                    }
                } else if owned {
                    // A new session's id only becomes routable with
                    // the reply, so its metadata waits for it.
                    flush = slot
                        .flush_held()
                        .into_iter()
                        .map(|update| (sid.to_owned(), update))
                        .collect();
                }
            }
        }
        Err(_) => {
            if let Some(sid) = session.as_deref() {
                // A newer attach may already own the slot; only the
                // attempt that still owns it may unwind it, and a
                // takeover keeps its own held updates and waiter.
                if wire.slots.get(sid).is_some_and(|slot| slot.setup == owner) {
                    if created {
                        wire.slots.remove(sid);
                    } else {
                        // Hand the slot back to the newest setup
                        // still in flight: its completion owns the
                        // replay marker and the parked metadata.
                        let previous = wire.newest_pending_setup(sid);
                        let attached = wire.slots.get(sid).is_some_and(|slot| slot.attached);
                        if let Some(slot) = wire.slots.get_mut(sid) {
                            match previous {
                                Some((previous_owner, previous_replay)) => {
                                    slot.setup = previous_owner;
                                    if !previous_replay {
                                        // No successor will emit its
                                        // `replay_complete`.
                                        slot.replay = None;
                                    }
                                }
                                None => {
                                    // This attempt's replay ends
                                    // here: an earlier load whose
                                    // success was stale skipped its
                                    // `replay_complete`, so leaving
                                    // the replay marker set would
                                    // keep resetting live chunks for
                                    // a replay that never completes.
                                    slot.replay = None;
                                    slot.flush_held();
                                    // No successor owns the parked
                                    // live updates: hand them back to
                                    // the still-attached client after
                                    // the error reply.
                                    if attached {
                                        deferred = slot.deferred.finish();
                                    }
                                }
                            }
                            // The previous attach is live only if
                            // one completed; while an earlier setup
                            // is still pending the slot stays
                            // `Attaching`.
                            if attached {
                                slot.attach = Attach::Attached;
                            }
                        }
                        // No attach ever completed and no setup is
                        // left to complete one: the failed attempts
                        // created this slot, so drop it.
                        if !attached && previous.is_none() {
                            wire.slots.remove(sid);
                        }
                    }
                }
            }
        }
    }
    let projected = result
        .map(|value| lifecycle::lifecycle_result(method, value))
        .map_err(|error| if two { errors::normalize(error) } else { error });
    // Cache the option list the host reported; `session/new` and
    // `session/load` already carry it in their reply.
    if let Ok(value) = &projected {
        if let Some(options) = value.get("configOptions").and_then(Value::as_array) {
            if let Some(slot) = sid
                .as_deref()
                .and_then(|session_id| wire.slots.get_mut(session_id))
            {
                slot.config = options.clone();
            }
        }
    }
    // A resume reply has no v1 `configOptions`; ask the host for the
    // list `session/new` returns and answer the client once it is
    // known, so both entry points expose the same configuration
    // surface. `session/load` already replayed it.
    if projected.is_ok() && session.is_some() && !replay {
        if let Some(session_id) = sid.as_deref() {
            let config_id = RequestId::String(uuid::Uuid::now_v7().to_string());
            wire.replies.insert(
                config_id.clone(),
                Reply::ConfigFetch {
                    client: id,
                    session: session_id.to_owned(),
                    result: projected,
                    deferred,
                },
            );
            stdio
                .park(IncomingMessage::Request {
                    id: config_id,
                    method: CONFIG.into(),
                    params: config::fetch_params(session_id),
                })
                .await;
            return Ok(());
        }
    }
    stdio.inner.send_response(id, projected).await?;
    if !deferred.is_empty() {
        if let Some(sid) = sid.as_deref() {
            if let Some(slot) = wire.slots.get_mut(sid) {
                stdio.flush_deferred(slot, sid, deferred).await?;
            }
        }
    }
    for (sid, update) in flush {
        let slot = wire.slots.get_mut(&sid);
        let projected = updates::project_update(update, false).unwrap_or_default();
        match slot {
            Some(slot) => stdio.emit(slot, &sid, projected).await?,
            None => {
                for update in projected {
                    stdio.update(&sid, update).await?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "replay_test.rs"]
mod tests;
