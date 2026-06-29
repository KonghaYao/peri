//! Effect execution context -- the main loop's side-effect executor.
//!
//! [`ApplyContext`] holds the I/O handles the main loop needs to execute
//! [`Effect`] variants produced by the state machine.  The state machine is
//! a pure function `(State, Event) -> (State, Vec<Effect>)` -- it performs no
//! I/O.  `ApplyContext::apply` is the **only** place that touches the terminal,
//! ACP transport, and clipboard.
//!
//! Design: `peri-tui-architecture.md` §8.3 -- ApplyContext holds terminal +
//! acp_client + clipboard.  Stateless -- all state lives in the state machine.

use std::io::{self, Write};
use std::time::Instant;

use ratatui::prelude::{CrosstermBackend, Terminal};
use tracing::warn;

use crate::acp_client::AcpTuiClient;
use crate::app::App;
use crate::runtime::effect::Effect;
use crate::ui;

/// Outcome of applying a single effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Effect applied successfully; continue the loop.
    Ok,
    /// The loop should exit.
    Quit,
}

/// Holds the I/O handles the main loop needs to execute effects.
///
/// Stateless -- all application state lives in the state machine.  This struct
/// only carries the external resources needed for side-effect execution:
/// - **terminal**: ratatui terminal for rendering.
/// - **acp_client**: ACP transport client for sending requests/notifications.
///
/// The main loop owns these resources and passes `&mut ApplyContext` to the
/// effect applier.  The `acp_client` is owned (not borrowed) because it is
/// `Clone` (internally `Arc<MpscClientTransport>`) and the state machine
/// never needs direct access -- all ACP communication flows through
/// [`Effect::SendToAcp`].
pub struct ApplyContext<'a> {
    /// The terminal used for rendering.
    pub terminal: &'a mut Terminal<CrosstermBackend<io::Stdout>>,
    /// The ACP client for sending requests and notifications upstream.
    pub acp_client: AcpTuiClient,
}

impl<'a> ApplyContext<'a> {
    /// Create a new `ApplyContext` holding the given terminal and ACP client.
    pub fn new(
        terminal: &'a mut Terminal<CrosstermBackend<io::Stdout>>,
        acp_client: AcpTuiClient,
    ) -> Self {
        Self {
            terminal,
            acp_client,
        }
    }

    /// Execute a single [`Effect`], returning the outcome.
    ///
    /// This is the **only** I/O path in the main loop.  Every side effect
    /// produced by the state machine flows through this method.
    pub async fn apply(&mut self, effect: Effect) -> ApplyOutcome {
        match effect {
            Effect::Render => {
                // Rendering is performed by the main loop caller via
                // `draw_if_needed` / `draw_now` to support throttling.
                // Here we just acknowledge the effect.
                ApplyOutcome::Ok
            }

            Effect::SendToAcp { method, params } => {
                // Route through the ACP client's raw request path.
                // Errors are logged but do not break the loop -- the state
                // machine will observe missing responses via timeout or
                // subsequent events.
                match self.acp_client.send_raw_request(&method, params).await {
                    Ok(response) => {
                        tracing::debug!(
                            method = %method,
                            "SendToAcp: request succeeded"
                        );
                        let _ = response; // Response is consumed by ACP protocol;
                                          // TUI does not process RPC responses
                                          // (all data arrives via notifications).
                    }
                    Err(e) => {
                        warn!(
                            method = %method,
                            error = %e,
                            "SendToAcp: request failed"
                        );
                    }
                }
                ApplyOutcome::Ok
            }

            Effect::CopyToClipboard(text) => {
                self.write_clipboard(&text);
                ApplyOutcome::Ok
            }

            Effect::Quit => ApplyOutcome::Quit,

            // App-level effects are handled by main_loop (need &mut App).
            // ApplyContext only carries I/O handles; reaching these arms means
            // the effect was passed via the "other" path, which shouldn't happen
            // because main_loop intercepts them first. Defensive no-op if it does.
            Effect::ShowNotification(_)
            | Effect::UpdateConfig { .. }
            | Effect::SwitchSession(_)
            | Effect::SubmitMessage { .. }
            | Effect::PollAgent
            | Effect::AdvanceSpinner
            | Effect::Scroll { .. }
            | Effect::AskUserScroll { .. }
            | Effect::MouseTextareaClick { .. }
            | Effect::MouseTextareaDrag { .. }
            | Effect::MouseRelease
            | Effect::PasteText { .. }
            | Effect::PushSystemNote(_)
            | Effect::OpenThreadWithFeedback { .. }
            | Effect::MemoryPanelOpenEditor
            | Effect::CycleModel
            | Effect::CycleProvider
            | Effect::CyclePermissionMode
            | Effect::FocusBgBar
            | Effect::ToggleDiff
            | Effect::PollWorkflow
            | Effect::InterruptAgent
            | Effect::ClearPendingMessages
            | Effect::ClearTextSelection
            | Effect::OpenRewindPrompt => ApplyOutcome::Ok,
        }
    }

    /// Write text to the system clipboard (best-effort).
    ///
    /// Uses platform-specific commands:
    /// - macOS: `pbcopy`
    /// - Linux (X11): `xclip -selection clipboard`
    /// - Linux (Wayland): `wl-copy`
    /// - Windows: `clip`
    fn write_clipboard(&self, text: &str) {
        #[cfg(target_os = "macos")]
        {
            use std::process::{Command, Stdio};
            if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
            }
        }

        #[cfg(target_os = "linux")]
        {
            use std::process::{Command, Stdio};
            // Try wl-copy (Wayland) first, fall back to xclip (X11).
            let cmd = if std::env::var("WAYLAND_DISPLAY").is_ok() {
                ("wl-copy", &[] as &[&str])
            } else {
                ("xclip", &["-selection", "clipboard"][..])
            };
            if let Ok(mut child) = Command::new(cmd.0)
                .args(cmd.1)
                .stdin(Stdio::piped())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
            }
        }

        #[cfg(target_os = "windows")]
        {
            use std::process::{Command, Stdio};
            if let Ok(mut child) = Command::new("clip").stdin(Stdio::piped()).spawn() {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
            }
        }
    }

    /// Perform a terminal draw if the throttle interval has elapsed.
    ///
    /// This mirrors the legacy loop's throttled render logic:
    /// - Always draw if the interval has elapsed.
    /// - Updates `last_render` to the current time after drawing.
    pub fn draw_if_needed(&mut self, app: &mut App, last_render: &mut Instant) {
        let now = Instant::now();
        if now.duration_since(*last_render) >= std::time::Duration::from_millis(33) {
            if let Err(e) = self.terminal.draw(|f| ui::main_ui::render(f, app)) {
                warn!(error = %e, "terminal draw failed");
            }
            *last_render = now;
        }
    }

    /// Unconditionally perform a terminal draw and reset the render timer.
    pub fn draw_now(&mut self, app: &mut App, last_render: &mut Instant) {
        if let Err(e) = self.terminal.draw(|f| ui::main_ui::render(f, app)) {
            warn!(error = %e, "terminal draw failed");
        }
        *last_render = Instant::now();
    }
}
