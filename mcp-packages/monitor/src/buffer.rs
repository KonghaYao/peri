//! In-memory event ring buffer for one monitor task.
//!
//! Events are line-oriented command output. The buffer assigns a monotonically
//! increasing `seq` and enforces the byte ceiling for one event. When capacity
//! is exceeded the oldest events are evicted; every surviving event carries the
//! number of events dropped immediately before it, and the read page reports
//! the cumulative dropped count.
//!
//! `monitor_read` treats `after_seq` as exclusive and returns `next_seq` as the
//! cursor to pass back. `next_seq` equals the last returned `seq` (or the
//! caller's cursor when nothing was returned). It never skips a future event.

use std::collections::VecDeque;
use std::sync::Arc;

#[cfg(test)]
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Maximum bytes retained as one event's text.
pub const MAX_EVENT_BYTES: usize = 16 * 1024;
/// Maximum events returned by one `monitor_read`.
pub const MAX_READ_EVENTS: usize = 500;
/// Maximum bytes returned by one `monitor_read`.
pub const MAX_READ_BYTES: usize = 128 * 1024;
/// Minimum per-task ring buffer size. A task must be able to retain one event.
pub const MIN_TASK_BYTES: usize = MAX_EVENT_BYTES;
/// Maximum per-task ring buffer size.
pub const MAX_TASK_BYTES: usize = 64 * 1024 * 1024;
/// Maximum per-task event count.
pub const MAX_TASK_EVENTS: usize = 100_000;

/// One captured command output line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorEvent {
    /// Monotonic sequence number assigned by the task buffer (starts at 1).
    pub seq: u64,
    /// Capture timestamp (UTC RFC 3339).
    pub ts: String,
    /// `stdout` or `stderr`.
    pub stream: String,
    /// Event kind. v1 only emits `line`.
    pub kind: String,
    /// Line content without the line terminator. Always untrusted data.
    pub text: String,
    /// Original line byte length, excluding the terminator.
    pub bytes: u64,
    /// The line was cut at [`MAX_EVENT_BYTES`].
    pub truncated: bool,
    /// Events evicted immediately before this event.
    pub dropped: u64,
    /// The line ended at EOF without a terminator.
    pub partial: bool,
    /// Always true: command output is never trusted instructions.
    pub untrusted: bool,
}

/// Raw line captured from one stream before buffer admission.
pub struct CapturedEvent {
    pub ts: String,
    pub stream: &'static str,
    pub kind: &'static str,
    pub text: String,
    pub bytes: u64,
    pub truncated: bool,
    pub partial: bool,
}

/// Admission sink for captured lines.
///
/// The production sink is the monitor task: it stores every line in the ring
/// buffer and also evaluates the alert. Capture workers must not write the
/// buffer directly, otherwise alert matches would be silently skipped.
pub trait EventAdmission: Send + Sync {
    fn admit(&self, event: CapturedEvent);
}

/// One read page from a task buffer.
pub struct EventPage {
    pub events: Vec<MonitorEvent>,
    /// Exclusive cursor to pass as `after_seq` next time.
    pub next_seq: u64,
    /// More matching events remained after this page.
    pub truncated: bool,
    /// Cumulative events evicted by the ring buffer.
    pub dropped: u64,
}

/// Bounded ring buffer for one task.
pub struct EventBuffer {
    events: VecDeque<MonitorEvent>,
    max_events: usize,
    max_bytes: usize,
    next_seq: u64,
    retained_bytes: usize,
    dropped: u64,
    pending_dropped: u64,
}

impl EventBuffer {
    pub fn new(max_events: usize, max_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            max_events: max_events.max(1),
            max_bytes: max_bytes.max(MIN_TASK_BYTES),
            next_seq: 1,
            retained_bytes: 0,
            dropped: 0,
            pending_dropped: 0,
        }
    }

    /// Append one captured event, evicting the oldest entries as needed.
    ///
    /// Returns the sequence number assigned to the event.
    pub fn push(&mut self, event: CapturedEvent) -> u64 {
        let (text, cut) = cap_text(event.text);
        let truncated = event.truncated || cut;
        let retained = text.len();

        while self.events.len() >= self.max_events
            || self.retained_bytes.saturating_add(retained) > self.max_bytes
        {
            let Some(evicted) = self.events.pop_front() else {
                break;
            };
            self.retained_bytes = self.retained_bytes.saturating_sub(evicted.text.len());
            self.dropped = self.dropped.saturating_add(1);
            self.pending_dropped = self.pending_dropped.saturating_add(1);
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.retained_bytes = self.retained_bytes.saturating_add(retained);
        self.events.push_back(MonitorEvent {
            seq,
            ts: event.ts,
            stream: event.stream.to_string(),
            kind: event.kind.to_string(),
            text,
            bytes: event.bytes,
            truncated,
            dropped: std::mem::take(&mut self.pending_dropped),
            partial: event.partial,
            untrusted: true,
        });
        seq
    }

    /// Read events strictly after `after_seq`.
    ///
    /// The page always includes at least one complete event when one is
    /// available: a `max_bytes` smaller than the next event cannot stall the
    /// cursor forever. Such a page is marked `truncated` and the caller can
    /// advance with `next_seq`.
    pub fn read(&self, after_seq: Option<u64>, limit: usize, max_bytes: usize) -> EventPage {
        let limit = limit.clamp(1, MAX_READ_EVENTS);
        let budget = max_bytes.clamp(1, MAX_READ_BYTES);
        let mut events = Vec::new();
        let mut used = 0usize;
        let mut truncated = false;

        for event in self
            .events
            .iter()
            .filter(|event| after_seq.is_none_or(|after| event.seq > after))
        {
            if events.len() == limit {
                truncated = true;
                break;
            }
            let exceeds_budget = used.saturating_add(event.text.len()) > budget;
            if exceeds_budget && !events.is_empty() {
                truncated = true;
                break;
            }
            if exceeds_budget {
                // The first event always fits by contract; flag the page so the
                // caller knows the byte budget was exceeded.
                truncated = true;
            }
            used = used.saturating_add(event.text.len());
            events.push(event.clone());
        }

        let next_seq = events
            .last()
            .map(|event| event.seq)
            .unwrap_or_else(|| after_seq.unwrap_or(0).min(self.last_assigned_seq()));

        EventPage {
            events,
            next_seq,
            truncated,
            dropped: self.dropped,
        }
    }

    pub fn last_assigned_seq(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Cut `text` at the event byte ceiling without splitting a UTF-8 character.
///
/// Returns the retained text and whether anything was removed.
fn cap_text(mut text: String) -> (String, bool) {
    if text.len() <= MAX_EVENT_BYTES {
        return (text, false);
    }
    let mut end = MAX_EVENT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

/// Capture newline-delimited output until EOF.
///
/// Long lines are capped at [`MAX_EVENT_BYTES`]; the remainder is discarded up
/// to the next newline so memory stays bounded. A final line without a newline
/// is emitted with `partial = true`.
pub async fn capture_lines<R, S>(mut reader: R, stream: &'static str, sink: Arc<S>)
where
    R: AsyncBufRead + Unpin,
    S: EventAdmission + 'static,
{
    let mut pending = Vec::new();
    let mut raw_bytes = 0u64;
    let mut truncated = false;

    loop {
        let chunk = match reader.fill_buf().await {
            Ok(chunk) => chunk,
            Err(error) => {
                tracing::debug!(stream, %error, "monitor output read failed");
                break;
            }
        };
        if chunk.is_empty() {
            if raw_bytes > 0 || !pending.is_empty() {
                emit_line(
                    &sink,
                    stream,
                    std::mem::take(&mut pending),
                    raw_bytes,
                    truncated,
                    true,
                );
            }
            break;
        }

        if let Some(position) = chunk.iter().position(|byte| *byte == b'\n') {
            let segment = &chunk[..position];
            raw_bytes = raw_bytes.saturating_add(segment.len() as u64);
            let room = MAX_EVENT_BYTES.saturating_sub(pending.len());
            let take = segment.len().min(room);
            pending.extend_from_slice(&segment[..take]);
            truncated |= take < segment.len();
            reader.consume(position + 1);
            emit_line(
                &sink,
                stream,
                std::mem::take(&mut pending),
                raw_bytes,
                truncated,
                false,
            );
            raw_bytes = 0;
            truncated = false;
        } else {
            raw_bytes = raw_bytes.saturating_add(chunk.len() as u64);
            let room = MAX_EVENT_BYTES.saturating_sub(pending.len());
            let take = chunk.len().min(room);
            pending.extend_from_slice(&chunk[..take]);
            truncated |= take < chunk.len();
            let consumed = chunk.len();
            reader.consume(consumed);
        }
    }
}

fn emit_line<S: EventAdmission + ?Sized>(
    sink: &Arc<S>,
    stream: &'static str,
    mut line: Vec<u8>,
    raw_bytes: u64,
    truncated: bool,
    partial: bool,
) {
    let mut bytes = raw_bytes;
    if line.last() == Some(&b'\r') {
        line.pop();
        bytes = bytes.saturating_sub(1);
    }
    let text = String::from_utf8_lossy(&line).into_owned();
    let (text, text_truncated) = cap_text(text);
    sink.admit(CapturedEvent {
        ts: peri_time::now_utc_rfc3339(),
        stream,
        kind: "line",
        text,
        bytes,
        truncated: truncated || text_truncated,
        partial,
    });
}

#[cfg(test)]
impl EventAdmission for Mutex<EventBuffer> {
    fn admit(&self, event: CapturedEvent) {
        self.lock().push(event);
    }
}

#[cfg(test)]
#[path = "buffer_test.rs"]
mod tests;
