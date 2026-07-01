//! Event types consumed by the TUI state machine.
//!
//! Every input source (crossterm polling, ACP notifications, system signals)
//! is translated into exactly one [`Event`] variant before entering the state
//! machine. The state machine's pure-function signature is
//! `(State, Event) -> (State, Vec<Effect>)`.
//!
//! Five event sources (design doc section 7.2):
//! - User input (Key / Mouse / Paste / Resize) -- keyboard collector task
//! - ACP events (decoded from `{event, data}`) -- ACP notifier task
//! - Periodic tick (~50 ms) -- keyboard collector task
//! - System signals (AcpDisconnected / SessionLoaded / Shutdown)
//!
//! This module mirrors [`crate::runtime::event_channel::TuiEvent`] but decodes
//! the raw `{event: String, data: Value}` payload into typed [`AcpEventData`]
//! variants so the state machine can match on concrete types instead of strings.
//!
//! S11 起 `AcpEventData` + decode 逻辑迁移到 `kit::acp_types`，本文件 re-export
//! 保持 legacy 路径兼容。

// Re-export AcpEventData（S11 起类型定义在 kit::acp_types）
pub use crate::kit::acp_types::AcpEventData;

// ---------------------------------------------------------------------------
// Event -- the single enum the state machine receives
// ---------------------------------------------------------------------------

/// Every input to the state machine.
///
/// Constructed by the main loop from [`crate::runtime::event_channel::TuiEvent`]
/// before being passed to the pure transition function. The main loop decodes
/// the raw ACP JSON into typed [`AcpEventData`] at this boundary.
#[derive(Debug, Clone)]
pub enum Event {
    /// Terminal key event (press / repeat / release).
    Key(ratatui::crossterm::event::KeyEvent),

    /// Terminal mouse event (click / scroll / drag / release / move).
    Mouse(ratatui::crossterm::event::MouseEvent),

    /// Bracketed-paste text. Line separators are already normalised to `\n`.
    Paste(String),

    /// Terminal resize with the new (columns, rows).
    Resize { width: u16, height: u16 },

    /// Periodic tick (~50 ms). Advances spinner frames, flushes throttle.
    Tick,

    /// A decoded ACP custom event -- one variant per protocol section 4 event.
    AcpEvent(AcpEventData),

    /// The ACP transport connection dropped (server crashed or disconnected).
    AcpDisconnected,

    /// A session load completed (used for session-switching transitions).
    SessionLoaded { session_id: String },

    /// Request the main loop to exit.
    Shutdown,

    /// Push a system note into the message view (e.g. `/agent` switch
    /// notification, `/model` failure). Routes through the state machine so
    /// the note lands in `state.view` (v2 source of truth) instead of the
    /// legacy `view_messages` Vec.
    PushSystemNote(String),

    /// Cron #24 P1 #2 — Push a user-submitted bubble into `state.view`.
    ///
    /// Used by `ask_user_confirm` to surface the user's answers in the message
    /// flow. Without this, answers were pushed only to v1 `view_messages`
    /// (which v2 render path doesn't read) and silently disappeared after the
    /// popup closed. Mirrors the `PushSystemNote` queue-and-drain pattern.
    PushUserBubble(String),
}

// ---------------------------------------------------------------------------
// Conversion from TuiEvent (raw channel type) to Event (state machine type)
// ---------------------------------------------------------------------------

impl From<crate::runtime::event_channel::TuiEvent> for Event {
    /// Convert the raw channel event into a state-machine event.
    ///
    /// The only non-trivial conversion is `TuiEvent::AcpEvent` which gets
    /// decoded via [`AcpEventData::decode`].
    fn from(raw: crate::runtime::event_channel::TuiEvent) -> Self {
        match raw {
            crate::runtime::event_channel::TuiEvent::Key(k) => Event::Key(k),
            crate::runtime::event_channel::TuiEvent::Mouse(m) => Event::Mouse(m),
            crate::runtime::event_channel::TuiEvent::Paste(s) => Event::Paste(s),
            crate::runtime::event_channel::TuiEvent::Resize(w, h) => Event::Resize {
                width: w,
                height: h,
            },
            crate::runtime::event_channel::TuiEvent::Tick => Event::Tick,
            crate::runtime::event_channel::TuiEvent::AcpEvent { event, data } => {
                Event::AcpEvent(AcpEventData::decode(&event, data))
            }
            crate::runtime::event_channel::TuiEvent::AcpDisconnected => Event::AcpDisconnected,
            crate::runtime::event_channel::TuiEvent::SessionLoaded { session_id } => {
                Event::SessionLoaded { session_id }
            }
            crate::runtime::event_channel::TuiEvent::Shutdown => Event::Shutdown,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_tui_event_key() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let tui_event = crate::runtime::event_channel::TuiEvent::Key(key);
        let event = Event::from(tui_event);
        match event {
            Event::Key(k) => assert_eq!(k.code, KeyCode::Char('a')),
            _ => panic!("expected Key event"),
        }
    }

    #[test]
    fn test_from_tui_event_acp_decoded() {
        let tui_event = crate::runtime::event_channel::TuiEvent::AcpEvent {
            event: "turn-done".to_owned(),
            data: serde_json::json!({}),
        };
        let event = Event::from(tui_event);
        match event {
            Event::AcpEvent(AcpEventData::TurnDone) => {}
            _ => panic!("expected AcpEvent::TurnDone"),
        }
    }

    #[test]
    fn test_from_tui_event_unknown_acp() {
        let tui_event = crate::runtime::event_channel::TuiEvent::AcpEvent {
            event: "brand-new-event".to_owned(),
            data: serde_json::json!({"x": 1}),
        };
        let event = Event::from(tui_event);
        match event {
            Event::AcpEvent(AcpEventData::Unknown { event, .. }) => {
                assert_eq!(event, "brand-new-event");
            }
            _ => panic!("expected Unknown ACP event"),
        }
    }

    #[test]
    fn test_from_tui_event_shutdown() {
        let tui_event = crate::runtime::event_channel::TuiEvent::Shutdown;
        let event = Event::from(tui_event);
        match event {
            Event::Shutdown => {}
            _ => panic!("expected Shutdown"),
        }
    }

    #[test]
    fn test_from_tui_event_resize() {
        let tui_event = crate::runtime::event_channel::TuiEvent::Resize(120, 40);
        let event = Event::from(tui_event);
        match event {
            Event::Resize { width, height } => {
                assert_eq!(width, 120);
                assert_eq!(height, 40);
            }
            _ => panic!("expected Resize"),
        }
    }
}
