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

use std::collections::HashMap;
use std::io::{self, Write};
use std::time::Instant;

use ratatui::prelude::{CrosstermBackend, Terminal};
use tracing::warn;

use crate::acp_client::AcpTuiClient;
use crate::app::App;
use crate::panel::read_context::ServiceRegistrySnapshot;
use crate::runtime::effect::Effect;
use crate::state_machine::state::PanelReadContext;
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
            | Effect::MouseTextareaClick { .. }
            | Effect::MouseTextareaDrag { .. }
            | Effect::MouseRelease
            | Effect::PasteText { .. }
            | Effect::PushSystemNote(_)
            | Effect::MemoryPanelOpenEditor { .. }
            | Effect::CycleModel
            | Effect::CycleProvider
            | Effect::CyclePermissionMode
            | Effect::FocusBgBar
            | Effect::ToggleDiff
            | Effect::PollWorkflow
            | Effect::ClearTextSelection
            | Effect::OpenPanel(_)
            | Effect::ClosePanel => ApplyOutcome::Ok,
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
    /// Throttled variant of [`draw_now`]; see its doc for v2 panel rendering.
    pub fn draw_if_needed(
        &mut self,
        app: &mut App,
        last_render: &mut Instant,
        state: &mut crate::state_machine::State,
    ) {
        let now = Instant::now();
        if now.duration_since(*last_render) >= std::time::Duration::from_millis(33) {
            self.draw_now(app, last_render, state);
        }
    }

    /// Unconditionally perform a terminal draw and reset the render timer.
    ///
    /// If `state` is [`State::Modal(ModalState::Panel(...))`], pre-computes the
    /// v2 panel height so the legacy layout reserves space, then renders the v2
    /// panel overlay in that reserved area.
    pub fn draw_now(
        &mut self,
        app: &mut App,
        last_render: &mut Instant,
        state: &mut crate::state_machine::State,
    ) {
        use crate::state_machine::{ModalKind, ModalState, State};
        // Clone view_models before the mutable borrow in the draw closure,
        // so we can pass them to build_v2_panel_read_context without
        // conflicting with the `if let State::Modal(..) = state` borrow.
        let view_models: Vec<peri_acp_types::view_model::ViewModel> = state.view_models().to_vec();
        // Collect V2 ViewModels for message area rendering:
        // committed view + current_turn (if streaming).
        let mut v2_vms: Vec<peri_acp_types::view_model::ViewModel> = view_models.clone();
        if let State::Streaming(s) = &mut *state {
            let turn_vms = s.current_turn.view_models().to_vec();
            if !turn_vms.is_empty() {
                v2_vms.extend(turn_vms);
            }
        }
        let v2_vms_ref: &[peri_acp_types::view_model::ViewModel] = &v2_vms;
        // Phase 2.3 step 8 + Phase 2.6：构造 probe 注入：
        // 1. SubAgent 运行时状态（is_running / total_steps / final_result / ...）
        // 2. 子内容（child_messages — 由 source_agent_id 路由实时累积）
        // 子 Agent 文本/工具/未来扩展全部通过 child_messages 权威源注入。
        let session = app.session_mgr.current();
        let probe = crate::app::SessionSubAgentProbe::new(session.subagent_status.clone());
        let status_probe: std::rc::Rc<dyn crate::render::view_render::SubAgentStatusProbe> =
            std::rc::Rc::new(probe);
        let draw_result = crate::render::view_render::with_status_probe(status_probe, || {
            self.terminal.draw(|f| {
                // Pre-compute v2 modal height (Panel or Interaction) so legacy
                // layout reserves space. Both kinds expose `desired_height`.
                let v2_panel_height = match &*state {
                    State::Modal(ModalState {
                        kind: ModalKind::Panel(panel),
                        ..
                    }) => Some(panel.desired_height(f.area().height, f.area().width)),
                    State::Modal(ModalState {
                        kind: ModalKind::Interaction(handler),
                        ..
                    }) => Some(handler.desired_height(f.area().height, f.area().width)),
                    _ => None,
                };
                ui::main_ui::render(f, app, v2_panel_height, Some(v2_vms_ref));
                // v2 Modal overlay: render in the area reserved by the layout.
                // Both Panel and Interaction variants read panel_area (set by
                // render_session_column when v2_panel_height is Some).
                if let State::Modal(ModalState { kind, .. }) = state {
                    let area = app
                        .session_mgr
                        .current()
                        .ui
                        .panel_area
                        .unwrap_or(ratatui::layout::Rect::new(0, 0, 80, 24));
                    match kind {
                        ModalKind::Panel(panel) => {
                            // Cron #30: refresh cached fields from live App
                            // before render. Default no-op; caching panels
                            // (Workflow/Cron/Tasks/ThreadBrowser/Mcp/Plugin)
                            // override to pull fresh data so the panel doesn't
                            // show a stale snapshot while open. Cursor/scroll
                            // state is preserved by each panel's refresh impl.
                            panel.refresh(app);
                            panel.render(f, area, &build_v2_panel_read_context(app, &view_models));
                        }
                        ModalKind::Interaction(handler) => {
                            handler.render(f, area);
                        }
                    }
                }
            })
        });
        if let Err(e) = draw_result {
            warn!(error = %e, "terminal draw failed");
        }
        *last_render = Instant::now();
    }
}

/// Build a [`PanelReadContext`] for v2 panel rendering from live App data.
///
/// `view_models` is a pre-cloned snapshot from `state.view_models()` — cloned
/// before the `terminal.draw()` closure to avoid borrow conflicts with the
/// `if let State::Modal(..) = state` mutable borrow inside the closure.
fn build_v2_panel_read_context<'a>(
    app: &'a App,
    view_models: &'a [peri_acp_types::view_model::ViewModel],
) -> PanelReadContext<'a> {
    use std::sync::LazyLock;

    static EMPTY_CACHE: LazyLock<HashMap<String, serde_json::Value>> = LazyLock::new(HashMap::new);

    // Thread-local snapshot store. Updated each draw tick; referenced within
    // the terminal.draw() closure via unsafe lifetime extension. Safe because
    // draws are single-threaded and the reference never escapes the closure.
    std::thread_local! {
        static SNAPSHOT: std::cell::RefCell<ServiceRegistrySnapshot> =
            std::cell::RefCell::new(ServiceRegistrySnapshot::new());
    }

    let session = app.session_mgr.current();

    // Leak the snapshot into the thread-local so it has 'static lifetime.
    // SAFETY: SNAPSHOT lives for the lifetime of the main thread. We borrow
    // it here and the reference does not escape this function's call stack.
    let services: &'a ServiceRegistrySnapshot = SNAPSHOT.with(|cell| {
        *cell.borrow_mut() = ServiceRegistrySnapshot::from_app(app);
        // SAFETY: the reference is valid for the duration of terminal.draw().
        // We never retain it beyond this function call.
        unsafe { &*cell.as_ptr() }
    });

    PanelReadContext {
        services,
        view_models,
        scroll_offset: session.ui.scroll_offset,
        area: session
            .ui
            .panel_area
            .unwrap_or(ratatui::layout::Rect::new(0, 0, 80, 24)),
        lc: &app.services.lc,
        acp_query_cache: &EMPTY_CACHE,
    }
}
