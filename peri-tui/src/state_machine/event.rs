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

use peri_acp_types::event_data::*;

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
// AcpEventData -- decoded ACP custom event
// ---------------------------------------------------------------------------

/// Decoded ACP custom event.
///
/// One variant per event name defined in the ACP protocol section 4
/// ("Event Directory", see `docs/design/peri-acp-protocol.md`).
///
/// The [`decode`](AcpEventData::decode) method maps a raw `{event, data}`
/// payload to the corresponding typed variant. Unknown event names are
/// captured as [`AcpEventData::Unknown`] for forward compatibility.
#[derive(Debug, Clone)]
pub enum AcpEventData {
    // -- §4.1 Streaming (high-frequency) ------------------------------------
    /// `"text-chunk"` -- incremental text for the current assistant bubble.
    TextChunk(TextChunk),

    /// `"reasoning-chunk"` -- incremental reasoning / thinking text.
    ReasoningChunk(ReasoningChunk),

    /// `"tool-started"` -- creates an in-progress tool card.
    ToolStarted(ToolStarted),

    /// `"tool-ended"` -- fills in the tool card result.
    ToolEnded(ToolEnded),

    // -- §4.2 Boundary (low-frequency) -------------------------------------
    /// `"view-commit"` -- complete ViewModel list, TUI replaces entire view.
    ViewCommit(ViewCommit),

    /// `"turn-done"` -- agent finished this turn (Streaming -> Idle).
    TurnDone,

    /// `"turn-interrupted"` -- agent was interrupted (user cancel / timeout).
    TurnInterrupted(TurnInterrupted),

    // -- §4.3 Status (status bar updates) ----------------------------------
    /// `"token-usage"` -- token consumption for the current turn.
    TokenUsage(TokenUsage),

    /// `"tool-count"` -- number of tool calls in the current turn.
    ToolCount(ToolCount),

    /// `"progress"` -- progress percentage with label.
    Progress(Progress),

    /// `"budget-warning"` -- context budget threshold crossed.
    BudgetWarning(BudgetWarning),

    /// `"system-notification"` -- system-level notification text with severity.
    SystemNotification(SystemNotification),

    // -- §4.4 Input assist -------------------------------------------------
    /// `"prediction"` -- input prediction suggestion (grey placeholder).
    Prediction(Prediction),

    /// `"file-suggestions"` -- @-mention file completion candidates.
    FileSuggestions(FileSuggestions),

    // -- §4.5 Interaction requests (require user decision) ------------------
    /// `"hitl-pending"` -- HITL tool approval request.
    HitlPending(HitlPending),

    /// `"ask-user"` -- multi-question form initiated by the agent.
    AskUser(AskUser),

    /// `"rewind-preview"` -- preview of changes that will be undone.
    RewindPreview(RewindPreview),

    /// `"oauth-needed"` -- MCP server authorization required.
    OauthNeeded(OauthNeeded),

    // -- §4.6 Structure (control message-area layout) ------------------------
    /// `"subagent-started"` -- sub-agent created, TUI opens a collapsible group.
    SubagentStarted(SubagentStarted),

    /// `"subagent-stopped"` -- sub-agent exited, TUI closes the group.
    SubagentStopped(SubagentStopped),

    /// Fallback for unknown / future event names.
    ///
    /// Keeps the raw event name and JSON data so the state machine can log or
    /// silently ignore new events without crashing.
    Unknown {
        event: String,
        data: serde_json::Value,
    },
}

impl AcpEventData {
    /// Decode a raw `{event, data}` payload into a typed [`AcpEventData`].
    ///
    /// Dispatches by event name (kebab-case string). On deserialization
    /// failure or unknown event name, falls back to [`AcpEventData::Unknown`].
    pub fn decode(event: &str, data: serde_json::Value) -> Self {
        match event {
            // §4.1 Streaming
            "text-chunk" => decode_or_unknown(event, data, AcpEventData::TextChunk),
            "reasoning-chunk" => decode_or_unknown(event, data, AcpEventData::ReasoningChunk),
            "tool-started" => decode_or_unknown(event, data, AcpEventData::ToolStarted),
            "tool-ended" => decode_or_unknown(event, data, AcpEventData::ToolEnded),

            // §4.2 Boundary
            "view-commit" => decode_or_unknown(event, data, AcpEventData::ViewCommit),
            "turn-done" => match serde_json::from_value::<TurnDone>(data.clone()) {
                Ok(_) => AcpEventData::TurnDone,
                Err(_) => AcpEventData::unknown(event, data),
            },
            "turn-interrupted" => decode_or_unknown(event, data, AcpEventData::TurnInterrupted),

            // §4.3 Status
            "token-usage" => decode_or_unknown(event, data, AcpEventData::TokenUsage),
            "tool-count" => decode_or_unknown(event, data, AcpEventData::ToolCount),
            "progress" => decode_or_unknown(event, data, AcpEventData::Progress),
            "budget-warning" => decode_or_unknown(event, data, AcpEventData::BudgetWarning),
            "system-notification" => {
                decode_or_unknown(event, data, AcpEventData::SystemNotification)
            }

            // §4.4 Input assist
            "prediction" => decode_or_unknown(event, data, AcpEventData::Prediction),
            "file-suggestions" => decode_or_unknown(event, data, AcpEventData::FileSuggestions),

            // §4.5 Interaction requests
            "hitl-pending" => decode_or_unknown(event, data, AcpEventData::HitlPending),
            "ask-user" => decode_or_unknown(event, data, AcpEventData::AskUser),
            "rewind-preview" => decode_or_unknown(event, data, AcpEventData::RewindPreview),
            "oauth-needed" => decode_or_unknown(event, data, AcpEventData::OauthNeeded),

            // §4.6 Structure
            "subagent-started" => decode_or_unknown(event, data, AcpEventData::SubagentStarted),
            "subagent-stopped" => decode_or_unknown(event, data, AcpEventData::SubagentStopped),

            // Unknown / future event names -- forward-compatible fallback.
            _ => AcpEventData::unknown(event, data),
        }
    }

    /// Helper to construct the [`AcpEventData::Unknown`] variant.
    fn unknown(event: &str, data: serde_json::Value) -> Self {
        AcpEventData::Unknown {
            event: event.to_owned(),
            data,
        }
    }
}

/// Decode `data` into `T` and apply the variant constructor, or fall back to
/// [`AcpEventData::Unknown`] with the original `data` preserved.
///
/// `data` is cloned up-front so the original is still available on the error
/// branch (serde consumes the value passed to `from_value`).
fn decode_or_unknown<T, F>(event: &str, data: serde_json::Value, ctor: F) -> AcpEventData
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> AcpEventData,
{
    match serde_json::from_value::<T>(data.clone()) {
        Ok(v) => ctor(v),
        Err(_) => AcpEventData::unknown(event, data),
    }
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

    // -- decode: §4.1 Streaming --------------------------------------------

    #[test]
    fn test_decode_text_chunk() {
        let data = serde_json::json!({"text": "hello", "agent_id": null});
        let decoded = AcpEventData::decode("text-chunk", data);
        match decoded {
            AcpEventData::TextChunk(tc) => {
                assert_eq!(tc.text, "hello");
                assert!(tc.agent_id.is_none());
            }
            _ => panic!("expected TextChunk, got {:?}", decoded),
        }
    }

    #[test]
    fn test_decode_reasoning_chunk() {
        let data = serde_json::json!({"text": "thinking...", "agent_id": "sa-1"});
        let decoded = AcpEventData::decode("reasoning-chunk", data);
        match decoded {
            AcpEventData::ReasoningChunk(rc) => {
                assert_eq!(rc.text, "thinking...");
                assert_eq!(rc.agent_id.as_deref(), Some("sa-1"));
            }
            _ => panic!("expected ReasoningChunk"),
        }
    }

    #[test]
    fn test_decode_tool_started() {
        let data = serde_json::json!({
            "tool_id": "tc-1",
            "tool_name": "Edit",
            "input_summary": "path: foo.rs"
        });
        let decoded = AcpEventData::decode("tool-started", data);
        match decoded {
            AcpEventData::ToolStarted(ts) => {
                assert_eq!(ts.tool_name, "Edit");
            }
            _ => panic!("expected ToolStarted"),
        }
    }

    #[test]
    fn test_decode_tool_ended() {
        let data = serde_json::json!({
            "tool_id": "tc-1",
            "output_summary": "ok",
            "is_error": false
        });
        let decoded = AcpEventData::decode("tool-ended", data);
        match decoded {
            AcpEventData::ToolEnded(te) => {
                assert!(!te.is_error);
            }
            _ => panic!("expected ToolEnded"),
        }
    }

    // -- decode: §4.2 Boundary ----------------------------------------------

    #[test]
    fn test_decode_view_commit() {
        let data = serde_json::json!({"view_models": []});
        let decoded = AcpEventData::decode("view-commit", data);
        match decoded {
            AcpEventData::ViewCommit(vc) => {
                assert!(vc.view_models.is_empty());
            }
            _ => panic!("expected ViewCommit"),
        }
    }

    #[test]
    fn test_decode_turn_done() {
        let decoded = AcpEventData::decode("turn-done", serde_json::json!({}));
        match decoded {
            AcpEventData::TurnDone => {}
            _ => panic!("expected TurnDone"),
        }
    }

    #[test]
    fn test_decode_turn_interrupted() {
        let data = serde_json::json!({"reason": "user cancelled"});
        let decoded = AcpEventData::decode("turn-interrupted", data);
        match decoded {
            AcpEventData::TurnInterrupted(ti) => {
                assert_eq!(ti.reason, "user cancelled");
            }
            _ => panic!("expected TurnInterrupted"),
        }
    }

    // -- decode: §4.3 Status ------------------------------------------------

    #[test]
    fn test_decode_token_usage() {
        let data = serde_json::json!({"input": 100, "output": 50});
        let decoded = AcpEventData::decode("token-usage", data);
        match decoded {
            AcpEventData::TokenUsage(tu) => {
                assert_eq!(tu.input, 100);
                assert_eq!(tu.output, 50);
            }
            _ => panic!("expected TokenUsage"),
        }
    }

    #[test]
    fn test_decode_tool_count() {
        let data = serde_json::json!({"count": 3});
        let decoded = AcpEventData::decode("tool-count", data);
        match decoded {
            AcpEventData::ToolCount(tc) => {
                assert_eq!(tc.count, 3);
            }
            _ => panic!("expected ToolCount"),
        }
    }

    #[test]
    fn test_decode_budget_warning() {
        let data = serde_json::json!({
            "used": 85000,
            "limit": 100000,
            "threshold": "0.85"
        });
        let decoded = AcpEventData::decode("budget-warning", data);
        match decoded {
            AcpEventData::BudgetWarning(bw) => {
                assert_eq!(bw.threshold, "0.85");
            }
            _ => panic!("expected BudgetWarning"),
        }
    }

    #[test]
    fn test_decode_system_notification() {
        let data = serde_json::json!({"text": "model switched", "level": "info"});
        let decoded = AcpEventData::decode("system-notification", data);
        match decoded {
            AcpEventData::SystemNotification(sn) => {
                assert_eq!(sn.level, "info");
            }
            _ => panic!("expected SystemNotification"),
        }
    }

    // -- decode: §4.4 Input assist -----------------------------------------

    #[test]
    fn test_decode_prediction() {
        let data = serde_json::json!({"text": "fix typo"});
        let decoded = AcpEventData::decode("prediction", data);
        match decoded {
            AcpEventData::Prediction(p) => {
                assert_eq!(p.text, "fix typo");
            }
            _ => panic!("expected Prediction"),
        }
    }

    #[test]
    fn test_decode_file_suggestions() {
        let data = serde_json::json!({"files": ["src/main.rs", "src/lib.rs"]});
        let decoded = AcpEventData::decode("file-suggestions", data);
        match decoded {
            AcpEventData::FileSuggestions(fs) => {
                assert_eq!(fs.files.len(), 2);
            }
            _ => panic!("expected FileSuggestions"),
        }
    }

    // -- decode: §4.5 Interaction requests ----------------------------------

    #[test]
    fn test_decode_hitl_pending_standalone() {
        let data = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {"path": "foo.rs"},
            "batch": null
        });
        let decoded = AcpEventData::decode("hitl-pending", data);
        match decoded {
            AcpEventData::HitlPending(hp) => {
                assert!(hp.batch.is_none());
            }
            _ => panic!("expected HitlPending"),
        }
    }

    #[test]
    fn test_decode_ask_user() {
        let data = serde_json::json!({
            "questions": [{
                "id": "q1",
                "header": "Choose",
                "question": "Which?",
                "options": [],
                "multi_select": false
            }]
        });
        let decoded = AcpEventData::decode("ask-user", data);
        match decoded {
            AcpEventData::AskUser(au) => {
                assert_eq!(au.questions.len(), 1);
            }
            _ => panic!("expected AskUser"),
        }
    }

    #[test]
    fn test_decode_rewind_preview() {
        let data = serde_json::json!({
            "files": [],
            "messages": []
        });
        let decoded = AcpEventData::decode("rewind-preview", data);
        match decoded {
            AcpEventData::RewindPreview(rp) => {
                assert!(rp.files.is_empty());
            }
            _ => panic!("expected RewindPreview"),
        }
    }

    #[test]
    fn test_decode_oauth_needed() {
        let data = serde_json::json!({
            "server_name": "github-mcp",
            "auth_url": "https://github.com/login/oauth"
        });
        let decoded = AcpEventData::decode("oauth-needed", data);
        match decoded {
            AcpEventData::OauthNeeded(on) => {
                assert_eq!(on.server_name, "github-mcp");
            }
            _ => panic!("expected OauthNeeded"),
        }
    }

    // -- decode: §4.6 Structure --------------------------------------------

    #[test]
    fn test_decode_subagent_started() {
        let data = serde_json::json!({
            "agent_id": "sa-1",
            "agent_name": "file-searcher"
        });
        let decoded = AcpEventData::decode("subagent-started", data);
        match decoded {
            AcpEventData::SubagentStarted(ss) => {
                assert_eq!(ss.agent_name, "file-searcher");
            }
            _ => panic!("expected SubagentStarted"),
        }
    }

    #[test]
    fn test_decode_subagent_stopped() {
        let data = serde_json::json!({"agent_id": "sa-1"});
        let decoded = AcpEventData::decode("subagent-stopped", data);
        match decoded {
            AcpEventData::SubagentStopped(ss) => {
                assert_eq!(ss.agent_id, "sa-1");
            }
            _ => panic!("expected SubagentStopped"),
        }
    }

    // -- Unknown / forward-compat -------------------------------------------

    #[test]
    fn test_decode_unknown_event_name() {
        let data = serde_json::json!({"foo": "bar"});
        let decoded = AcpEventData::decode("future-event", data);
        match decoded {
            AcpEventData::Unknown { event, data } => {
                assert_eq!(event, "future-event");
                assert_eq!(data["foo"], "bar");
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn test_decode_malformed_data_falls_to_unknown() {
        // Valid event name but completely wrong data shape.
        let data = serde_json::json!("not an object");
        let decoded = AcpEventData::decode("text-chunk", data);
        match decoded {
            AcpEventData::Unknown { event, .. } => {
                assert_eq!(event, "text-chunk");
            }
            _ => panic!("expected Unknown for malformed data"),
        }
    }

    // -- From<TuiEvent> conversion ------------------------------------------

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
