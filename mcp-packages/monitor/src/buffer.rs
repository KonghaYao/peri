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
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

#[cfg(test)]
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

#[cfg(test)]
#[path = "buffer_test.rs"]
mod tests;

#[path = "buffer/event_buffer.rs"]
mod event_buffer;
pub use event_buffer::capture_lines;
pub use event_buffer::CapturedEvent;
pub use event_buffer::EventAdmission;
pub use event_buffer::EventBuffer;
pub use event_buffer::EventPage;
pub use event_buffer::MonitorEvent;
pub use event_buffer::MAX_EVENT_BYTES;
pub use event_buffer::MAX_READ_BYTES;
pub use event_buffer::MAX_READ_EVENTS;
pub use event_buffer::MAX_TASK_BYTES;
pub use event_buffer::MAX_TASK_EVENTS;
pub use event_buffer::MIN_TASK_BYTES;
