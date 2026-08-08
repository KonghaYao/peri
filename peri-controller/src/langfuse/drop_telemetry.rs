use std::collections::{BTreeMap, VecDeque};

use langfuse_client::{IngestionEvent, LangfuseError};
use parking_lot::Mutex;

/// 仅按 IngestionEvent 枚举变体归类；绝不检查或保留事件 payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LangfuseEventKind {
    Trace,
    Session,
    Observation,
    Span,
    Generation,
    Event,
    Score,
    SdkLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LangfuseDropReason {
    DropNewQueueFull,
    BatcherClosed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LangfuseDropSnapshot {
    pub trace_id: String,
    pub total: u64,
    pub by_event_kind: BTreeMap<LangfuseEventKind, u64>,
    pub by_reason: BTreeMap<LangfuseDropReason, u64>,
}

impl LangfuseEventKind {
    pub fn from_event(event: &IngestionEvent) -> Self {
        match event {
            IngestionEvent::TraceCreate { .. } => Self::Trace,
            IngestionEvent::SessionCreate { .. } | IngestionEvent::SessionUpdate { .. } => {
                Self::Session
            }
            IngestionEvent::ObservationCreate { .. } | IngestionEvent::ObservationUpdate { .. } => {
                Self::Observation
            }
            IngestionEvent::SpanCreate { .. } | IngestionEvent::SpanUpdate { .. } => Self::Span,
            IngestionEvent::GenerationCreate { .. } | IngestionEvent::GenerationUpdate { .. } => {
                Self::Generation
            }
            IngestionEvent::EventCreate { .. } => Self::Event,
            IngestionEvent::ScoreCreate { .. } => Self::Score,
            IngestionEvent::SdkLog { .. } => Self::SdkLog,
        }
    }
}

impl LangfuseDropReason {
    pub fn from_error(error: &LangfuseError) -> Option<Self> {
        match error {
            LangfuseError::QueueFull => Some(Self::DropNewQueueFull),
            LangfuseError::ChannelClosed => Some(Self::BatcherClosed),
            _ => None,
        }
    }
}

/// 有界、进程内的背压遥测。仅保存 trace ID 和计数，不保存事件或任何 payload。
pub struct LangfuseDropRegistry {
    capacity: usize,
    entries: Mutex<DropEntries>,
}

#[derive(Default)]
struct DropEntries {
    order: VecDeque<String>,
    snapshots: BTreeMap<String, LangfuseDropSnapshot>,
}

impl LangfuseDropRegistry {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::new(DropEntries::default()),
        }
    }

    pub fn record(
        &self,
        trace_id: &str,
        event_kind: LangfuseEventKind,
        reason: LangfuseDropReason,
    ) {
        let mut entries = self.entries.lock();
        if !entries.snapshots.contains_key(trace_id) {
            if entries.order.len() == self.capacity {
                if let Some(oldest) = entries.order.pop_front() {
                    entries.snapshots.remove(&oldest);
                }
            }
            entries.order.push_back(trace_id.to_string());
            entries.snapshots.insert(
                trace_id.to_string(),
                LangfuseDropSnapshot {
                    trace_id: trace_id.to_string(),
                    ..Default::default()
                },
            );
        }
        let snapshot = entries
            .snapshots
            .get_mut(trace_id)
            .expect("entry inserted above");
        snapshot.total += 1;
        *snapshot.by_event_kind.entry(event_kind).or_default() += 1;
        *snapshot.by_reason.entry(reason).or_default() += 1;
    }

    pub fn snapshot(&self, trace_id: &str) -> Option<LangfuseDropSnapshot> {
        self.entries.lock().snapshots.get(trace_id).cloned()
    }
}

impl Default for LangfuseDropRegistry {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[cfg(test)]
#[path = "drop_telemetry_test.rs"]
mod tests;
