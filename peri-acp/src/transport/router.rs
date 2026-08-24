//! Cancellation-safe request/response ownership shared by all ACP transports.

use std::{collections::HashMap, fmt, sync::Arc};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::types::{AcpError, IncomingMessage, RequestId};

struct PendingEntry {
    owner: Arc<()>,
    sender: oneshot::Sender<Result<Value, AcpError>>,
}

struct RouterState {
    next_id: i64,
    pending: HashMap<i64, PendingEntry>,
    terminal_error: Option<AcpError>,
}

/// Shared request-response lifecycle for transport implementations.
///
/// Registration, response dispatch, caller cancellation, and terminal close all
/// claim an entry while holding one state lock. The per-registration owner token
/// prevents a stale handle from deleting a later request if an ID is reused.
#[derive(Clone)]
pub(crate) struct RequestRouter {
    state: Arc<Mutex<RouterState>>,
    closed: CancellationToken,
}

/// Owned registration for one pending request.
///
/// Dropping this value synchronously releases the registration. This makes any
/// future that owns it cancellation-safe without spawning async cleanup.
pub(crate) struct PendingRequest {
    id: i64,
    owner: Arc<()>,
    receiver: Option<oneshot::Receiver<Result<Value, AcpError>>>,
    router: RequestRouter,
}

impl fmt::Debug for PendingRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingRequest")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl PendingRequest {
    pub(crate) fn id(&self) -> i64 {
        self.id
    }

    pub(crate) async fn wait(mut self) -> Result<Value, AcpError> {
        self.receiver
            .take()
            .expect("pending request receiver is consumed exactly once")
            .await
            .unwrap_or_else(|_| Err(transport_closed_error()))
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        let mut state = self.router.state.lock();
        let still_owned = state
            .pending
            .get(&self.id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.owner, &self.owner));
        if still_owned {
            state.pending.remove(&self.id);
        }
    }
}

impl RequestRouter {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RouterState {
                next_id: 1,
                pending: HashMap::new(),
                terminal_error: None,
            })),
            closed: CancellationToken::new(),
        }
    }

    /// Atomically rejects terminal routers or registers a new owned request.
    pub(crate) fn register(&self) -> Result<PendingRequest, AcpError> {
        let mut state = self.state.lock();
        if let Some(error) = &state.terminal_error {
            return Err(error.clone());
        }

        let id = allocate_id(&mut state);
        let owner = Arc::new(());
        let (sender, receiver) = oneshot::channel();
        state.pending.insert(
            id,
            PendingEntry {
                owner: Arc::clone(&owner),
                sender,
            },
        );
        drop(state);

        Ok(PendingRequest {
            id,
            owner,
            receiver: Some(receiver),
            router: self.clone(),
        })
    }

    pub(crate) fn ensure_open(&self) -> Result<(), AcpError> {
        self.state.lock().terminal_error.clone().map_or(Ok(()), Err)
    }

    /// Routes a matching numeric response exactly once.
    pub(crate) fn dispatch(&self, msg: &IncomingMessage) -> bool {
        let IncomingMessage::Response {
            id: RequestId::Number(id),
            result,
        } = msg
        else {
            return false;
        };

        let entry = self.state.lock().pending.remove(id);
        if let Some(entry) = entry {
            let _ = entry.sender.send(result.clone());
            true
        } else {
            false
        }
    }

    /// Idempotently transitions the router to its canonical terminal state.
    ///
    /// The terminal flag and pending drain are one atomic state transition; a
    /// concurrent registration therefore either belongs to the drain or fails.
    pub(crate) fn close(&self) {
        let (error, pending) = {
            let mut state = self.state.lock();
            if state.terminal_error.is_some() {
                return;
            }
            let error = transport_closed_error();
            state.terminal_error = Some(error.clone());
            let pending = state
                .pending
                .drain()
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>();
            (error, pending)
        };

        self.closed.cancel();
        for entry in pending {
            let _ = entry.sender.send(Err(error.clone()));
        }
    }

    /// Completes immediately after close, including when close preceded this wait.
    pub(crate) async fn wait_closed(&self) {
        self.closed.cancelled().await;
    }
}

fn allocate_id(state: &mut RouterState) -> i64 {
    loop {
        let candidate = state.next_id.max(1);
        state.next_id = if candidate == i64::MAX {
            1
        } else {
            candidate + 1
        };
        if !state.pending.contains_key(&candidate) {
            return candidate;
        }
    }
}

pub(crate) fn transport_closed_error() -> AcpError {
    AcpError::new(-32603, "Transport closed")
}

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;
