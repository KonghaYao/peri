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

    // ── Mouse textarea interaction ────────────────────────────────────────
    /// Left-click on textarea: set cursor position and start selection.
    MouseTextareaClick {
        row: u16,
        column: u16,
    },
    /// Drag on textarea: extend selection while dragging.
    MouseTextareaDrag {
        row: u16,
        column: u16,
    },
    /// Left button up: release scrollbar drag / textarea selection.
    MouseRelease,

    // ── ACP protocol ─────────────────────────────────────────────────────
    /// Send an ACP method call or custom event.
    SendToAcp {
        method: String,
        params: serde_json::Value,
    },

    // ── Clipboard ────────────────────────────────────────────────────────
    CopyToClipboard(String),

    // ── Paste routing (setup wizard → interaction popup → textarea) ──────
    /// Paste text routed by the main loop to setup wizard, interaction popup,
    /// or legacy textarea (fallback).
    PasteText {
        text: String,
    },

    // ── Agent control ────────────────────────────────────────────────────
    // Note: InterruptAgent / ClearPendingMessages were removed — Ctrl+C and
    // Esc-during-loading are handled by the legacy keyboard module directly
    // via &mut App. When the keyboard path is Effect-ized (B3 MigrateInput),
    // these will be re-added with proper SM emit points.

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
    /// Open a v2 panel via state machine transition: Idle → Modal(Panel).
    OpenPanel(crate::app::PanelKind),
    /// Close the current v2 Modal panel: Modal(Panel) → Idle.
    ClosePanel,

    // ── App-level state mutations ────────────────────────────────────────
    /// Cycle to the next model alias (opus → sonnet → haiku → opus).
    CycleModel,
    /// Cycle to the next provider.
    CycleProvider,
    /// Cycle permission mode (default → acceptEdits → bypassPermissions → ...).
    CyclePermissionMode,
    /// Focus the background agent bar.
    FocusBgBar,
    /// Toggle inline diff rendering.
    ToggleDiff,
    /// Poll workflow runs for the workflow panel.
    PollWorkflow,
    /// Clear text selection (on terminal resize).
    ClearTextSelection,
    // Note: OpenRewindPrompt was removed — the legacy keyboard module handles
    // Esc directly via app.open_rewind_prompt(). Phase 1.4-rewind will wire
    // the v2 RewindHandler via HandlerOutput::Submit → proper ACP method.

    // ── System notes ─────────────────────────────────────────────────────
    /// Push a system note (model switch, compact, etc.) into the view.
    PushSystemNote(String),

    // ── Thread / session ─────────────────────────────────────────────────
    // Note: OpenThreadWithFeedback was removed — no SM emitter existed; the
    // thread browser panel will emit its own PanelEffect when wired to v2.
    /// Open the memory panel with system editor.
    MemoryPanelOpenEditor {
        path: std::path::PathBuf,
    },

    // ── App lifecycle ────────────────────────────────────────────────────
    /// Exit the app.
    Quit,

    // ── Internal loop mechanics ──────────────────────────────────────────
    /// Drain pending v2 notes from App into the state machine view.
    DrainPendingNotes,
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
    fn test_effect_memory_panel_open_editor() {
        let e = Effect::MemoryPanelOpenEditor {
            path: std::path::PathBuf::from("/tmp/test.md"),
        };
        assert!(matches!(e, Effect::MemoryPanelOpenEditor { .. }));
    }
}
