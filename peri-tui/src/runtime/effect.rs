/// Side-effect instruction produced by the state machine, executed by the main loop.
///
/// The state machine is a pure function `(State, Event) -> (State, Vec<Effect>)`.
/// These effects carry instructions for the main loop to execute I/O (render, ACP
/// send, clipboard) and App-level mutations (submit, scroll, etc.).
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    // ── Rendering ────────────────────────────────────────────────────────
    /// Trigger a terminal redraw. De-duplicated by the main loop.
    Render,

    // ── Agent communication ──────────────────────────────────────────────
    /// Submit user input to the agent.
    SubmitMessage {
        text: String,
    },
    /// Poll ACP notifications and v2 message queue (per-tick).
    PollAgent,
    /// Advance the loading spinner frame.
    AdvanceSpinner,

    // ── Scrolling ────────────────────────────────────────────────────────
    /// Scroll the main message viewport (positive = down, negative = up).
    Scroll {
        delta: i32,
    },
    /// Scroll the AskUser questions panel.
    AskUserScroll {
        delta: i32,
    },

    // ── ACP protocol ─────────────────────────────────────────────────────
    /// Send an ACP method call or custom event.
    SendToAcp {
        method: String,
        params: serde_json::Value,
    },

    // ── Clipboard ────────────────────────────────────────────────────────
    CopyToClipboard(String),

    // ── Panel / interaction side-effects ─────────────────────────────────
    /// Show a transient notification.
    ShowNotification(String),
    /// Update config key-value pair.
    UpdateConfig {
        key: String,
        value: String,
    },
    /// Switch to another session.
    SwitchSession(String),

    // ── System notes ─────────────────────────────────────────────────────
    /// Push a system note (model switch, compact, etc.) into the view.
    PushSystemNote(String),

    // ── Thread / session ─────────────────────────────────────────────────
    /// Open a thread browser entry with user feedback context.
    OpenThreadWithFeedback {
        thread_id: String,
    },
    /// Open the memory panel with system editor.
    MemoryPanelOpenEditor,

    // ── App lifecycle ────────────────────────────────────────────────────
    /// Exit the app.
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effect_submit_message_carries_input() {
        let e = Effect::SubmitMessage {
            text: "hello".into(),
        };
        match e {
            Effect::SubmitMessage { text } => assert_eq!(text, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_effect_poll_agent_no_payload() {
        let e = Effect::PollAgent;
        assert!(matches!(e, Effect::PollAgent));
    }

    #[test]
    fn test_effect_scroll_carries_delta() {
        let e = Effect::Scroll { delta: -3 };
        match e {
            Effect::Scroll { delta } => assert_eq!(delta, -3),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_effect_advance_spinner() {
        assert!(matches!(Effect::AdvanceSpinner, Effect::AdvanceSpinner));
    }

    #[test]
    fn test_effect_push_system_note() {
        let e = Effect::PushSystemNote("compact done".into());
        match e {
            Effect::PushSystemNote(msg) => assert_eq!(msg, "compact done"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_effect_open_thread_with_feedback() {
        let e = Effect::OpenThreadWithFeedback {
            thread_id: "t1".into(),
        };
        match e {
            Effect::OpenThreadWithFeedback { thread_id } => assert_eq!(thread_id, "t1"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_effect_memory_panel_open_editor() {
        assert!(matches!(
            Effect::MemoryPanelOpenEditor,
            Effect::MemoryPanelOpenEditor
        ));
    }
}
