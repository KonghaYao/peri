//! Single unbounded event channel that merges all input sources into one stream.
//!
//! Five categories of events flow through this channel:
//! - User input (Key / Mouse / Paste / Resize) -- from the keyboard collector task
//! - ACP events (`{event, data}` format) -- from the ACP notifier task
//! - Periodic ticks (~50 ms) -- from the keyboard collector task
//! - System signals (AcpDisconnected / SessionLoaded / Shutdown)

use serde_json::Value;
use tokio::sync::mpsc;

/// Single event type pushed into the TUI event channel.
///
/// Every input source (crossterm polling, ACP notifications, system signals)
/// is translated into exactly one `TuiEvent` variant before entering the
/// channel, so the main loop only needs to `recv` from one place.
#[derive(Debug, Clone)]
pub enum TuiEvent {
    /// A terminal key event (press / repeat / release).
    Key(ratatui::crossterm::event::KeyEvent),

    /// A terminal mouse event (click / scroll / drag / release / move).
    Mouse(ratatui::crossterm::event::MouseEvent),

    /// Bracketed-paste text.  The string may contain newlines; line
    /// separators are already normalised to `\n` by the collector.
    Paste(String),

    /// Terminal resize.  Carries the new (columns, rows).
    Resize(u16, u16),

    /// Periodic tick (~50 ms).  Used to advance spinner animations and
    /// flush throttle timers in the main loop.
    Tick,

    /// An ACP notification forwarded from the `AcpNotifier` background task.
    /// The `event` field is the kebab-case event name (e.g. `"text-chunk"`),
    /// and `data` is the full JSON payload.
    AcpEvent { event: String, data: Value },

    /// The ACP transport connection dropped (e.g. ACP server crashed).
    AcpDisconnected,

    /// A session load completed (used for session switching transitions).
    SessionLoaded { session_id: String },

    /// Request the main loop to exit.
    Shutdown,
}

/// Unbounded sender half of the TUI event channel.
pub type EventTx = mpsc::UnboundedSender<TuiEvent>;

/// Unbounded receiver half of the TUI event channel.
pub type EventRx = mpsc::UnboundedReceiver<TuiEvent>;

/// Create a new (sender, receiver) pair for the TUI event channel.
pub fn channel() -> (EventTx, EventRx) {
    mpsc::unbounded_channel()
}
