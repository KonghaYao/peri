//! Connection and per-session state for the ACP v2 stdio preview.
//!
//! One connection holds a slot per attached session. Each slot owns the
//! foreground state machine, its turn epoch, the blocking-request count that
//! aggregates permissions/elicitations, its prompt receipts and (from S4) replay.
//! All decisions are made while holding the connection mutex so the published
//! order matches the decision order.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::transport::types::{AcpError, RequestId};

use super::capabilities::ClientCapabilities;
use super::prompts::Prompts;

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
    /// Turn identity. Bumped on start, end, cancel and teardown so a stale
    /// blocking-request completion cannot publish a state for a newer turn.
    pub epoch: u64,
    pub foreground: Foreground,
    /// Unfinished blocking peer requests (permission/elicitation) of this turn.
    pub blocking: usize,
    pub execution_failed: bool,
    pub prompts: Prompts,
    pub replay: Option<Replay>,
    held: Vec<Value>,
}

impl Slot {
    pub(super) fn new(attach: Attach) -> Self {
        Self {
            attach,
            epoch: 0,
            foreground: Foreground::Idle,
            blocking: 0,
            execution_failed: false,
            prompts: Prompts::default(),
            replay: None,
            held: Vec::new(),
        }
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
    pub(super) fn unblock(&mut self, epoch: u64) -> bool {
        if self.epoch != epoch || self.foreground != Foreground::RequiresAction {
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
        /// The slot was created by this setup and must be dropped on failure.
        created: bool,
    },
    Close(String),
    Delete(String),
    Input(String),
    Withdraw(String),
    Cancel,
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
}

impl Connection {
    pub(super) fn is_two(&self) -> bool {
        matches!(self.version, Version::Two)
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
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
