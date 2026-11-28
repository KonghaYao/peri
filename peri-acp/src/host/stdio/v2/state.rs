//! Connection and per-session state for the ACP v2 stdio preview.
//!
//! One connection holds a slot per attached session. Each slot owns the
//! foreground state machine, its turn epoch, the blocking-request count that
//! aggregates permissions/elicitations, its prompt receipts and (from S4) replay.
//! All decisions are made while holding the connection mutex so the published
//! order matches the decision order.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::host::history_page::{self, SessionLoadMode};
use crate::transport::types::{AcpError, RequestId};

use super::capabilities::ClientCapabilities;
use super::errors;
use super::prompts::Prompts;
use super::replay::Deferred;

/// Upper bound on notifications parked while a setup request is in flight. The
/// hold exists to avoid losing a handful of metadata updates, not to buffer a
/// stream: past the bound the adapter drops and keeps serving.
pub(super) const MAX_HELD_UPDATES: usize = 256;

#[derive(Default)]
pub(super) enum Version {
    #[default]
    Unset,
    One,
    Two,
}

/// Aggregate foreground activity of one session (pinned `StateUpdate`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Foreground {
    #[default]
    Idle,
    Running,
    RequiresAction,
}

impl Foreground {
    pub(super) fn is_idle(self) -> bool {
        matches!(self, Self::Idle)
    }
}

/// Whether the client can already address this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Attach {
    /// Setup completed: notifications flow straight through.
    Attached,
    /// A setup request is in flight: metadata updates are parked until then.
    Attaching,
}

/// Replay state of one session. Presence means replay is streaming.
#[derive(Default)]
pub(super) struct Replay {
    /// Chunk identities already streamed, so a replayed message restarts with a
    /// replacement boundary instead of appending twice.
    pub messages: HashSet<(String, String)>,
}

pub(super) struct Slot {
    pub attach: Attach,
    /// Whether a setup on this slot ever completed. Only a completed attach may
    /// restore `Attached` after a failed takeover; while every setup is still
    /// pending the slot stays `Attaching`.
    pub attached: bool,
    /// Identity of this slot object within the connection. A slot dropped by
    /// close/delete and re-created by a later attach gets a new value, so a
    /// completion captured before the close cannot act on its replacement.
    pub incarnation: u64,
    /// Identity of the newest setup that bound this slot. A failed setup only
    /// undoes the slot when it still owns it.
    pub setup: u64,
    /// The newest setup that completed an attach, owning or stale. A failed
    /// takeover cannot hand the slot back to a pending attempt older than this
    /// one: an attach already superseded it.
    pub attached_setup: u64,
    /// Turn identity. Bumped on start, end, cancel and teardown so a stale
    /// blocking-request completion cannot publish a state for a newer turn.
    pub epoch: u64,
    pub foreground: Foreground,
    /// Unfinished blocking peer requests (permission/elicitation) of this turn.
    pub blocking: usize,
    pub execution_failed: bool,
    pub prompts: Prompts,
    /// Replay state of each setup attempt that asked for a cursor replay,
    /// keyed by its setup id. Keeping it per attempt lets a failed takeover
    /// hand the replay (and each message's replacement boundary) back intact.
    pub replays: HashMap<u64, Replay>,
    /// The setup attempt whose replay currently streams, if any. A cursorless
    /// attempt stops the stream but leaves earlier attempts' state in `replays`.
    pub replay_owner: Option<u64>,
    /// The newest `state_update` suppressed while live updates were held for a
    /// replay. A successful replay reports it through its authoritative
    /// snapshot; a failed one publishes it, so the client still learns that the
    /// turn ended and why.
    pub pending_state: Option<Value>,
    /// Live updates parked while this slot replays and until the owning resume
    /// reply flushes them. Presence and bounds are owned by [`Deferred`].
    pub deferred: Deferred,
    /// The session's configuration options as last reported by the host, in
    /// their v2 shape. `session/set_config_option` validates against this list so
    /// an unknown id or an out-of-range value is refused instead of being
    /// silently accepted by the v1 handler.
    pub config: Vec<Value>,
    held: Vec<Value>,
}

impl Slot {
    pub(super) fn new(attach: Attach) -> Self {
        Self {
            attach,
            attached: false,
            incarnation: 0,
            setup: 0,
            attached_setup: 0,
            epoch: 0,
            foreground: Foreground::Idle,
            blocking: 0,
            execution_failed: false,
            prompts: Prompts::default(),
            replays: HashMap::new(),
            replay_owner: None,
            pending_state: None,
            deferred: Deferred::default(),
            config: Vec::new(),
            held: Vec::new(),
        }
    }

    /// Bind this setup attempt's replay mode.
    ///
    /// A cursor replay starts a fresh stream owned by the attempt. A cursorless
    /// attempt stops the active stream without discarding earlier attempts'
    /// chunk identities, so a handback can resume exactly where it was.
    pub(super) fn start_setup_replay(&mut self, owner: u64, replay: bool) {
        self.replays.remove(&owner);
        if replay {
            self.replays.insert(owner, Replay::default());
            self.replay_owner = Some(owner);
            self.deferred.begin();
        } else {
            self.replay_owner = None;
        }
    }

    /// Drop one attempt's replay state; returns whether it was streaming.
    pub(super) fn end_setup_replay(&mut self, owner: u64) -> bool {
        self.replays.remove(&owner);
        if self.replay_owner == Some(owner) {
            self.replay_owner = None;
            true
        } else {
            false
        }
    }

    /// Whether a replay stream is active on this slot.
    pub(super) fn is_replaying(&self) -> bool {
        self.replay_owner
            .is_some_and(|owner| self.replays.contains_key(&owner))
    }

    /// The active attempt's replay state.
    pub(super) fn active_replay_mut(&mut self) -> Option<&mut Replay> {
        let owner = self.replay_owner?;
        self.replays.get_mut(&owner)
    }

    /// Take the suppressed state snapshot and keep its stop reason only when it
    /// describes the foreground state about to be published.
    pub(super) fn take_suppressed_stop_reason(&mut self, foreground: Foreground) -> Option<String> {
        let state = self.pending_state.take()?;
        let published = state.get("state").and_then(Value::as_str);
        let matches = match foreground {
            Foreground::Idle => published == Some("idle"),
            Foreground::Running => published == Some("running"),
            Foreground::RequiresAction => published == Some("requires_action"),
        };
        if !matches {
            return None;
        }
        state
            .get("stopReason")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// Foreground work started. Returns whether `running` must be published.
    pub(super) fn begin_turn(&mut self) -> bool {
        if !self.foreground.is_idle() {
            return false;
        }
        self.epoch += 1;
        self.blocking = 0;
        self.execution_failed = false;
        self.foreground = Foreground::Running;
        true
    }

    /// A blocking peer request is about to be sent.
    ///
    /// Returns the epoch its completion must match and whether
    /// `requires_action` must be published. Idle sessions are not blocked on
    /// user action: only foreground work warrants the state.
    pub(super) fn block(&mut self) -> Option<(u64, bool)> {
        match self.foreground {
            Foreground::Idle => None,
            Foreground::Running => {
                self.blocking = 1;
                self.foreground = Foreground::RequiresAction;
                Some((self.epoch, true))
            }
            // Another permission/elicitation of the same turn: the session is
            // already `requires_action` and stays there until the last one ends.
            Foreground::RequiresAction => {
                self.blocking += 1;
                Some((self.epoch, false))
            }
        }
    }

    /// A blocking peer request completed (any outcome). Returns whether
    /// `running` must be published because this was the last one outstanding.
    ///
    /// The completion names both the slot incarnation and the turn epoch it was
    /// captured under; either changing means the request belonged to a slot or
    /// turn this session no longer has.
    pub(super) fn unblock(&mut self, incarnation: u64, epoch: u64) -> bool {
        if self.incarnation != incarnation
            || self.epoch != epoch
            || self.foreground != Foreground::RequiresAction
        {
            return false;
        }
        self.blocking = self.blocking.saturating_sub(1);
        if self.blocking > 0 {
            return false;
        }
        self.foreground = Foreground::Running;
        true
    }

    /// The foreground turn ended. Returns whether `idle` must be published.
    pub(super) fn end_turn(&mut self) -> bool {
        let published = !self.foreground.is_idle();
        self.foreground = Foreground::Idle;
        self.blocking = 0;
        self.epoch += 1;
        published
    }

    /// The whole turn is being cancelled: stale completions must not publish.
    pub(super) fn cancel_turn(&mut self) {
        self.epoch += 1;
        self.blocking = 0;
    }

    pub(super) fn hold(&mut self, update: Value) {
        if self.held.len() < MAX_HELD_UPDATES {
            self.held.push(update);
        } else {
            tracing::warn!(
                held = self.held.len(),
                "ACP v2 attach hold is full; dropping a parked session update"
            );
        }
    }

    pub(super) fn flush_held(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.held)
    }
}

/// Outcome of routing one inbound v2 request.
pub(super) enum Routed {
    /// Forward to the shared host under an internal (or v1) method name.
    ///
    /// `id` is the request id the host must answer with: the client's own id for
    /// passthrough methods, a fresh internal id where the adapter keeps the
    /// client request open (prompt admission).
    Forward {
        id: RequestId,
        method: String,
        params: Value,
    },
    /// The adapter answered on its own (rejection or a protocol-level no-op).
    Answered(Result<Value, AcpError>),
    /// The request is parked inside the adapter (idempotent prompt waiter).
    Parked,
}

/// A request this adapter answered on the host's behalf.
pub(super) enum Reply {
    /// The client declarations become connection state only once the host
    /// confirms the initialization.
    Initialize(ClientCapabilities),
    Setup {
        /// `Some` for resume (the id is known up front), `None` for new.
        session: Option<String>,
        replay: bool,
        /// The setup that bound the slot. A failure only unwinds this attempt.
        owner: u64,
    },
    Close(String),
    Delete(String),
    Input(String),
    Withdraw(String),
    Cancel,
    /// `session/set_config_option`: the reply's `configOptions` need the v2
    /// identity field.
    ConfigOptions,
    /// A `session/resume` reply parked until the host answers the configuration
    /// fetch; the client request is answered once its option list is known.
    ConfigFetch {
        client: RequestId,
        session: String,
        /// The already-projected reply that is waiting for `configOptions`.
        result: Result<Value, AcpError>,
        /// Live updates taken from the slot when the setup reply was ready; they
        /// are written after the client's reply, per the replay contract.
        deferred: Vec<Value>,
    },
    /// `session/list`: the preview-only internal query; the client's reply is
    /// written once the host has paged storage.
    SessionList {
        client: RequestId,
    },
    /// A v1 setup whose success binds the connection's single session.
    V1Setup {
        /// `Some` for `session/load|resume`, `None` for `session/new|fork`.
        session: Option<String>,
    },
    /// A v1 close, which releases that binding when it succeeds.
    V1Close(String),
}

/// The v1 one-session gate's answer to an inbound setup request.
pub(super) enum V1Admission {
    /// Not a setup request: forward untouched.
    Pass,
    /// Forward; the reply binds or releases the connection's session.
    Tracked(Reply),
    /// Refused before the host sees it, with the host's own error shape.
    Refused(AcpError),
}

#[derive(Default)]
pub(super) struct Connection {
    pub version: Version,
    /// A non-initialize request has arrived. Repeating `initialize` is only
    /// allowed before this point.
    pub initialized: bool,
    /// What the client declared in `initialize`.
    pub client: ClientCapabilities,
    pub replies: HashMap<RequestId, Reply>,
    pub slots: HashMap<String, Slot>,
    /// Source of [`Slot::incarnation`]. Monotonic per connection.
    next_incarnation: u64,
    /// The v1 session this connection currently holds.
    ///
    /// The preview wrapper reports `allows_concurrent_sessions` before the
    /// version is negotiated, so the shared host skips its historical
    /// one-session gate for every connection through it. A v1 client gets that
    /// gate here instead.
    v1_current: Option<String>,
    /// A v1 setup is in flight and may bind the connection.
    v1_setup: bool,
}

impl Connection {
    pub(super) fn is_two(&self) -> bool {
        matches!(self.version, Version::Two)
    }

    /// Insert a slot if absent; a new slot gets a fresh connection-wide identity.
    pub(super) fn insert_slot(&mut self, session_id: &str, attach: Attach) -> &mut Slot {
        use std::collections::hash_map::Entry;

        match self.slots.entry(session_id.to_owned()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                self.next_incarnation = self.next_incarnation.wrapping_add(1);
                let incarnation = self.next_incarnation;
                let mut slot = Slot::new(attach);
                slot.incarnation = incarnation;
                entry.insert(slot)
            }
        }
    }

    /// The newest setup still in flight for one session, if any.
    ///
    /// A failed setup hands the slot back to this attempt, so its completion
    /// performs the flush and the `replay_complete` the failed one no longer
    /// can. The current reply is removed from `replies` before its result is
    /// applied, so only older attempts for the same session are returned.
    pub(super) fn newest_pending_setup(&self, session_id: &str) -> Option<(u64, bool)> {
        self.replies
            .values()
            .filter_map(|reply| match reply {
                Reply::Setup {
                    session: Some(session),
                    owner,
                    replay,
                    ..
                } if session == session_id => Some((*owner, *replay)),
                _ => None,
            })
            .max_by_key(|(owner, _)| *owner)
    }

    /// Reproduce the host's one-session-per-connection rule for a v1 client.
    ///
    /// Mirrors `host::connection_context`: a setup is refused while another one
    /// is in flight; a connection that holds a session may only re-attach that
    /// same session; close releases it. v2 connections never reach this.
    ///
    /// A read-only `session/load` (a page cursor or a `history` window) is not a
    /// setup: the host classifies `session/load` before it reserves or binds
    /// anything, answers those from storage, and never attaches a session. Such
    /// a request therefore passes untouched — otherwise a v1 connection bound to
    /// A could not page B's history, and a successful page would bind the
    /// connection to B. Capability truth stays with the host, which re-runs the
    /// same classifier with the connection's real declarations. A request the
    /// classifier cannot treat as read-only history keeps the setup admission:
    /// an unbound connection sees the host's own error shape, while a connection
    /// bound to another session (or with a setup in flight) is refused here
    /// first.
    pub(super) fn admit_v1_setup(&mut self, method: &str, params: &Value) -> V1Admission {
        if method == "session/load" && read_only_history(params) {
            return V1Admission::Pass;
        }
        let restore = match method {
            "session/new" | "session/fork" => None,
            "session/load" | "session/resume" => Some(
                params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ),
            _ => return V1Admission::Pass,
        };
        if self.v1_setup {
            return V1Admission::Refused(errors::session_setup_in_progress());
        }
        if self.v1_current.is_some() {
            let reuse = restore
                .as_ref()
                .is_some_and(|requested| requested.as_deref() == self.v1_current.as_deref());
            return if reuse {
                V1Admission::Pass
            } else {
                V1Admission::Refused(errors::session_switch_required())
            };
        }
        self.v1_setup = true;
        V1Admission::Tracked(Reply::V1Setup {
            session: restore.flatten(),
        })
    }

    pub(super) fn commit_v1_setup(&mut self, session_id: &str) {
        self.v1_setup = false;
        self.v1_current = Some(session_id.to_owned());
    }

    pub(super) fn fail_v1_setup(&mut self) {
        self.v1_setup = false;
    }

    pub(super) fn release_v1_session(&mut self, session_id: &str) {
        if self.v1_current.as_deref() == Some(session_id) {
            self.v1_current = None;
        }
    }

    /// The slot of a session this connection may still address.
    pub(super) fn slot(&self, session_id: &str) -> Option<&Slot> {
        self.slots
            .get(session_id)
            .filter(|slot| slot.attach == Attach::Attached)
    }

    /// The session whose in-flight prompt owns this JSON-RPC request id.
    pub(super) fn prompt_target(&self, id: &RequestId) -> Option<(String, String)> {
        self.slots.iter().find_map(|(session_id, slot)| {
            slot.prompts
                .target(id)
                .map(|(_, input_id)| (session_id.clone(), input_id))
        })
    }

    /// Reject every parked prompt waiter of one receipt id, in any slot.
    pub(super) fn reject_prompt(&mut self, input_id: &str) -> Vec<RequestId> {
        self.slots
            .values_mut()
            .flat_map(|slot| slot.prompts.reject(input_id))
            .collect()
    }

    /// Acknowledge every parked prompt waiter of an already-inserted message.
    pub(super) fn acknowledge_prompt(&mut self, input_id: &str) -> Vec<RequestId> {
        self.slots
            .values_mut()
            .flat_map(|slot| slot.prompts.acknowledge(input_id))
            .collect()
    }
}

/// Whether a `session/load` request is a read-only history read.
///
/// Runs the host's only classifier with both capabilities assumed: the wrapper
/// decides "is this a setup at all", not whether this connection may page. The
/// host re-classifies with the negotiated capabilities and owns that answer.
fn read_only_history(params: &Value) -> bool {
    matches!(
        history_page::classify(params, true, true),
        Ok(SessionLoadMode::HistoryPage(_) | SessionLoadMode::HistoryWindow { .. })
    )
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
