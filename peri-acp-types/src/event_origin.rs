//! Runtime-only provenance for a child execution's display events.

use crate::{event::ExecutorEvent, session::ExecutionBinding};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

static NEXT_CHILD_RUN_ORDER: AtomicU64 = AtomicU64::new(1);

/// An execution identity captured by its producer, never reconstructed by a consumer.
/// The sequence orders emitted display events; it does not prove canonical coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildEventOrigin {
    pub child_thread_id: String,
    pub execution: ExecutionBinding,
    pub run_order: u64,
    pub sequence: u64,
}

impl ChildEventOrigin {
    pub fn valid(&self) -> bool {
        !self.child_thread_id.is_empty()
            && self.child_thread_id.len() <= 128
            && !self.execution.attempt_id.as_str().is_empty()
            && self.execution.attempt_id.as_str().len() <= 128
            && self.run_order != 0
            && self.sequence != 0
    }
}

/// A wire carrier, not a persisted transcript or an execution recovery record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginatedExecutorEvent {
    pub origin: ChildEventOrigin,
    pub event: Box<ExecutorEvent>,
}

/// Shared by start, forwarding, normal stop and abort cleanup for exactly one run.
#[derive(Clone)]
pub struct ChildEventStream {
    origin: ChildEventOrigin,
    sequence: Arc<AtomicU64>,
}

impl ChildEventStream {
    pub fn new(child_thread_id: String, execution: ExecutionBinding) -> Self {
        Self {
            origin: ChildEventOrigin {
                child_thread_id,
                execution,
                run_order: NEXT_CHILD_RUN_ORDER.fetch_add(1, Ordering::Relaxed),
                sequence: 0,
            },
            sequence: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn wrap(&self, event: ExecutorEvent) -> ExecutorEvent {
        // Nested children have their own producer; an ancestor must not overwrite it.
        if matches!(event, ExecutorEvent::Originated(_)) {
            return event;
        }
        let mut origin = self.origin.clone();
        origin.sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        ExecutorEvent::Originated(Box::new(OriginatedExecutorEvent {
            origin,
            event: Box::new(event),
        }))
    }
}

impl ExecutorEvent {
    pub fn child_origin(&self) -> Option<&ChildEventOrigin> {
        match self {
            Self::Originated(event) => Some(&event.origin),
            _ => None,
        }
    }

    pub fn payload(&self) -> &Self {
        match self {
            Self::Originated(event) => event.event.payload(),
            _ => self,
        }
    }
}

#[cfg(test)]
#[path = "event_origin_test.rs"]
mod tests;
