//! Main loop: recv event → state_machine::handle (pure) + thin_handle (legacy) → apply effects → loop.
//!
//! P2 Cutover state: the loop **simultaneously** drives the new pure
//! state machine ([`crate::state_machine::handle`]) and the legacy
//! [`thin_handle`] glue. Effects from both paths are merged (Render
//! de-duplicated) before execution.
//!
//! - The state machine is the **future** authoritative path. Its `State`
//!   persists across events; `ViewStore` accumulates view-commits; transitions
//!   are pure functions with zero I/O.
//! - `thin_handle` is the **current** source of truth for rendering (reads
//!   `message_pipeline`) and for any behavior not yet ported to the state
//!   machine (panels, interaction popups, mouse selection).
//!
//! Once P3 (panels) and P5 (rendering rewrite) land, `thin_handle` will be
//! deleted and the state machine becomes the sole driver.

use std::time::Duration;

use ratatui::crossterm::event::MouseEvent;
use tracing::debug;

use crate::app::App;
use crate::event::keyboard;
use crate::event::Action;
use crate::runtime::apply_context::{ApplyContext, ApplyOutcome};
use crate::runtime::effect::Effect;
use crate::runtime::event_channel::{EventRx, TuiEvent};
use crate::state_machine::{handle as state_machine_handle, Event as SmEvent, IdleState, State};

/// Target frame interval for loading-spinner animation (~30 FPS).
const TARGET_FRAME_INTERVAL: Duration = Duration::from_millis(33);

// ── Public entry point ──────────────────────────────────────────────────────

/// Run the v2 main loop until the channel closes or an effect requests Quit.
///
/// The loop is the **only** place that reads from the event channel and the
/// **only** place that performs I/O (terminal draw, ACP send, clipboard).
///
/// P2 Cutover: drives both the pure state machine and the legacy
/// [`thin_handle`] glue. Effects are merged; `Render` is de-duplicated.
pub async fn run(mut rx: EventRx, ctx: &mut ApplyContext<'_>, app: &mut App) -> anyhow::Result<()> {
    let mut last_render = std::time::Instant::now();

    // v2 state machine state. Persists across events. Initial = Idle.
    let mut state: State = State::Idle(IdleState::default());

    while let Some(event) = rx.recv().await {
        let is_tick = matches!(event, TuiEvent::Tick);

        // ── 1a. Drive the pure state machine ────────────────────────────
        // Convert TuiEvent → SmEvent (decode ACP {event, data} into typed
        // AcpEventData variants) and dispatch to the transition function.
        let sm_event: SmEvent = event.clone().into();
        let (new_state, sm_effects) = state_machine_handle(state, sm_event);
        state = new_state;

        // ── 1b. Drive legacy thin_handle (fallback for unported paths) ──
        let legacy_effects = thin_handle(app, event);

        // ── 1c. Merge effects (Render de-duplicated) ────────────────────
        let mut effects: Vec<Effect> = sm_effects;
        for e in legacy_effects {
            if !effects.contains(&e) {
                effects.push(e);
            }
        }

        // ── 2. Execute effects ─────────────────────────────────────────
        let mut quit = false;
        let mut needs_render = false;
        for effect in effects {
            match effect {
                Effect::Render => needs_render = true,
                Effect::Quit => {
                    quit = true;
                    break;
                }
                // ── Agent communication ────────────────────────────
                Effect::SubmitMessage { text } => {
                    app.submit_message(text);
                    needs_render = true;
                }
                Effect::PollAgent => {
                    app.poll_agent();
                }
                Effect::AdvanceSpinner => {
                    app.session_mgr.current_mut().spinner_state.advance_tick();
                }
                // ── Scrolling ──────────────────────────────────────
                Effect::Scroll { delta } => match delta.cmp(&0) {
                    std::cmp::Ordering::Greater => app.scroll_down(),
                    std::cmp::Ordering::Less => app.scroll_up(),
                    std::cmp::Ordering::Equal => {}
                },
                Effect::AskUserScroll { delta } => {
                    app.ask_user_scroll(delta as i16);
                }
                // ── Agent control ─────────────────────────────────
                Effect::InterruptAgent => {
                    app.interrupt();
                }
                Effect::ClearPendingMessages => {
                    app.session_mgr
                        .current_mut()
                        .messages
                        .pending_messages
                        .clear();
                }
                // ── App-level effects (P3 Integration) ─────────────
                Effect::ShowNotification(text) => {
                    tracing::info!(notification = %text, "ShowNotification");
                    needs_render = true;
                }
                Effect::UpdateConfig { key, value } => {
                    tracing::info!(key = %key, value = %value, "UpdateConfig");
                    needs_render = true;
                }
                Effect::SwitchSession(session_id) => {
                    tracing::info!(session_id = %session_id, "SwitchSession");
                    needs_render = true;
                }
                // ── App state mutations ────────────────────────────
                Effect::CycleModel => {
                    let cfg_arc = app.services.peri_config.clone();
                    let mut cfg = cfg_arc.write();
                    let aliases = ["opus", "sonnet", "haiku"];
                    let current = cfg.config.active_alias.as_str();
                    let idx = aliases.iter().position(|&a| a == current).unwrap_or(0);
                    let next = aliases[(idx + 1) % aliases.len()];
                    cfg.config.active_alias = next.to_string();
                    if let Err(e) = crate::app::App::save_config(
                        &cfg,
                        app.services.config_path_override.as_deref(),
                    ) {
                        use crate::app::MessageViewModel;
                        app.session_mgr.current_mut().messages.view_messages.push(
                            MessageViewModel::system(app.services.lc.tr_args(
                                "config-save-failed",
                                &[("error".into(), e.to_string().into())],
                            )),
                        );
                    }
                    if let Some(p) = crate::app::agent::LlmProvider::from_config(&cfg) {
                        app.services.provider_name = p.display_name().to_string();
                        app.services.model_name = p.model_name().to_string();
                    }
                    if let Some(ref acp_client) = app.acp_client {
                        let acp = acp_client.clone();
                        tokio::spawn(async move {
                            let _ = acp.set_config_option("model", next).await;
                        });
                    }
                    app.global_ui.model_highlight_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                    needs_render = true;
                }
                Effect::CycleProvider => {
                    let cfg_arc = app.services.peri_config.clone();
                    let mut cfg = cfg_arc.write();
                    let providers_len = cfg.config.providers.len();
                    if providers_len > 1 {
                        let current_id = cfg.config.active_provider_id.as_str();
                        let next_id = {
                            let providers = &cfg.config.providers;
                            let idx = providers
                                .iter()
                                .position(|p| p.id == current_id)
                                .unwrap_or(0);
                            let next_idx = (idx + 1) % providers.len();
                            providers[next_idx].id.clone()
                        };
                        cfg.config.active_provider_id = next_id;
                        if let Some(p) = crate::app::agent::LlmProvider::from_config(&cfg) {
                            app.services.provider_name = p.display_name().to_string();
                            app.services.model_name = p.model_name().to_string();
                        }
                        if let Err(e) = crate::app::App::save_config(
                            &cfg,
                            app.services.config_path_override.as_deref(),
                        ) {
                            use crate::app::MessageViewModel;
                            app.session_mgr.current_mut().messages.view_messages.push(
                                MessageViewModel::system(app.services.lc.tr_args(
                                    "config-save-failed",
                                    &[("error".into(), e.to_string().into())],
                                )),
                            );
                        }
                        app.global_ui.provider_highlight_until = Some(
                            std::time::Instant::now() + std::time::Duration::from_millis(2000),
                        );
                    }
                    needs_render = true;
                }
                Effect::CyclePermissionMode => {
                    app.services.permission_mode.cycle();
                    app.global_ui.mode_highlight_until =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                    needs_render = true;
                }
                Effect::FocusBgBar => {
                    if !app.session_mgr.current_mut().background_agents.is_empty() {
                        app.session_mgr.current_mut().ui.bg_bar_cursor = Some(0);
                    }
                    needs_render = true;
                }
                Effect::ToggleDiff => {
                    if app.global_ui.oauth_prompt.is_none() {
                        app.toggle_diff();
                    }
                    needs_render = true;
                }
                Effect::PollWorkflow => {
                    if app.workflow_polling_active {
                        app.poll_workflow_runs();
                    }
                }
                Effect::ClearTextSelection => {
                    app.session_mgr.current_mut().ui.text_selection.clear();
                }
                Effect::OpenRewindPrompt => {
                    app.open_rewind_prompt();
                    needs_render = true;
                }
                // ── System / Thread / Memory ───────────────────────
                Effect::PushSystemNote(msg) => {
                    app.push_system_note(msg);
                    needs_render = true;
                }
                Effect::OpenThreadWithFeedback { thread_id } => {
                    app.open_thread_with_feedback(thread_id);
                    needs_render = true;
                }
                Effect::MemoryPanelOpenEditor => {
                    if let Err(e) = app.memory_panel_open_editor() {
                        tracing::warn!(error = %e, "MemoryPanelOpenEditor failed");
                    }
                    needs_render = true;
                }
                // I/O effects handled by ApplyContext (terminal / ACP / clipboard).
                other => match ctx.apply(other).await {
                    ApplyOutcome::Quit => {
                        quit = true;
                        break;
                    }
                    ApplyOutcome::Ok => {}
                },
            }
        }
        if quit {
            break;
        }

        // ── 3. Check App-level quit flag (/exit, /quit commands) ────────
        if app.global_ui.quit_requested {
            break;
        }

        // ── 4. Render ───────────────────────────────────────────────────
        // User events (Key/Mouse/Paste/Resize) and ACP events always
        // trigger an immediate redraw.  Tick events are throttled to
        // TARGET_FRAME_INTERVAL to cap the spinner animation at ~30 FPS.
        if needs_render {
            if is_tick {
                let now = std::time::Instant::now();
                if now.duration_since(last_render) >= TARGET_FRAME_INTERVAL {
                    ctx.draw_now(app, &mut last_render);
                }
            } else {
                ctx.draw_now(app, &mut last_render);
            }
        }
    }

    Ok(())
}

// ── Thin-shell state machine (P1 glue) ─────────────────────────────────────

/// Map a single [`TuiEvent`] to legacy `App` method calls, returning a list of
/// [`Effect`]s to be executed by the main loop.
///
/// Key invariants:
/// - **Never skips events**: every event is dispatched to the appropriate App
///   method.  Unknown AcpEvent JSON is logged but still returns `[Render]`.
/// - **Always returns at least one effect** (typically `Render`) so the caller
///   never has a no-op iteration.
/// - **`submit_message` sets loading=true synchronously** before returning,
///   so any subsequent drain in the same tick naturally short-circuits.
fn thin_handle(app: &mut App, event: TuiEvent) -> Vec<Effect> {
    // ── Periodic tick ─────────────────────────────────────────────────
    match event {
        TuiEvent::Tick => {
            // Spinner, poll agent, poll workflow are now handled by the
            // state machine (idle.rs Tick → AdvanceSpinner/PollAgent/PollWorkflow).
            // Legacy Tick is a no-op; Render is de-duplicated by the caller.
            vec![Effect::Render]
        }

        // ── User input: key press ───────────────────────────────────────
        TuiEvent::Key(key_event) => {
            // Shortcuts now handled by the state machine (idle.rs).
            // Skip legacy dispatch to avoid double-execution.
            if is_sm_handled_shortcut(&key_event) {
                return vec![Effect::Render];
            }
            // Delegate to the existing keyboard handler.
            match keyboard::handle_key_event(app, key_event) {
                Ok(Some(Action::Quit)) => vec![Effect::Quit],
                Ok(Some(Action::Submit(input))) => {
                    app.submit_message(input);
                    vec![Effect::Render]
                }
                Ok(Some(Action::Redraw)) => vec![Effect::Render],
                Ok(None) => vec![Effect::Render],
                Err(e) => {
                    tracing::warn!(error = %e, "keyboard handler returned error");
                    vec![Effect::Render]
                }
            }
        }

        // ── User input: mouse ───────────────────────────────────────────
        TuiEvent::Mouse(mouse_event) => {
            // Delegate to the legacy mouse handling inside `handle_event`.
            // We call the inner logic directly to avoid going through the
            // crossterm poll path.
            handle_mouse_event(app, mouse_event);
            vec![Effect::Render]
        }

        // ── User input: paste ──────────────────────────────────────────
        TuiEvent::Paste(text) => {
            handle_paste_event(app, &text);
            vec![Effect::Render]
        }

        // ── User input: resize ──────────────────────────────────────────
        TuiEvent::Resize(_cols, _rows) => {
            app.session_mgr.current_mut().ui.text_selection.clear();
            vec![Effect::Render]
        }

        // ── ACP notification (converted from AcpNotification) ───────────
        TuiEvent::AcpEvent {
            ref event,
            ref data,
        } => handle_acp_event(app, event, data),

        // ── ACP transport disconnected ──────────────────────────────────
        TuiEvent::AcpDisconnected => {
            // Transport drop — notify user but keep the loop running.
            // The legacy code does not have explicit handling for this
            // either; the ACP server crash is observed implicitly via
            // missing Done events.
            tracing::warn!("ACP transport disconnected");
            app.push_system_note(
                "ACP connection lost. Agent responses may not arrive.".to_string(),
            );
            vec![Effect::Render]
        }

        // ── Session loaded (future: session switching transition) ────────
        TuiEvent::SessionLoaded { session_id } => {
            debug!(session_id = %session_id, "SessionLoaded event (no-op in P1)");
            vec![Effect::Render]
        }

        // ── Shutdown signal ─────────────────────────────────────────────
        TuiEvent::Shutdown => vec![Effect::Quit],
    }
}

// ── Legacy event delegation helpers ──────────────────────────────────────────
//
// These functions replicate the relevant branches of the legacy
// `handle_event()` in `event/mod.rs`, calling the same `App` methods.
// They are intentionally verbose rather than trying to reconstruct a
// `CrosstermEvent` and calling `handle_event()` directly, because the
// latter function calls `event::poll()` internally (for mouse coalescing
// etc.) which we cannot invoke from the v2 loop.

fn handle_mouse_event(app: &mut App, mouse: MouseEvent) {
    // Minimal P1 dispatch: delegate to the existing mouse handling logic.
    // The full mouse handling (scroll, click, drag, selection, clipboard)
    // lives in event/mod.rs::handle_event's Mouse branch.
    // For P1, we reconstruct a CrosstermEvent::Mouse and call a
    // dedicated helper that contains just the mouse dispatch logic.
    //
    // NOTE: The legacy code uses EVENT_STASH for mouse coalescing.
    // In the v2 architecture, mouse coalescing happens in the
    // keyboard_collector (it does NOT — the collector only filters
    // FocusGained/FocusLost).  Mouse coalescing is deferred to P2
    // when the keyboard collector is enhanced, or we add coalescing
    // here as a follow-up.
    //
    // For P1, we directly call the legacy mouse handler by
    // constructing a CrosstermEvent and delegating.  Since we cannot
    // call `event::handle_event` (it calls `event::poll()`), we
    // replicate the mouse branch inline.

    use ratatui::crossterm::event::MouseButton;
    use ratatui::crossterm::event::MouseEventKind;

    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            // AskUser popup scroll takes priority
            if let Some(crate::app::InteractionPrompt::Questions(_)) =
                app.session_mgr.current_mut().agent.interaction_prompt
            {
                if let Some(area) = app.session_mgr.current_mut().ui.panel_area {
                    use crate::event::mouse;
                    if mouse::mouse_in_rect(&mouse, area) {
                        let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                            -3
                        } else {
                            3
                        };
                        app.ask_user_scroll(delta);
                        return;
                    }
                }
            }

            match mouse.kind {
                MouseEventKind::ScrollUp => app.scroll_up(),
                MouseEventKind::ScrollDown => app.scroll_down(),
                _ => unreachable!(),
            }
        }
        MouseEventKind::Down(MouseButton::Left)
            // Textarea selection start
            if !app.is_interaction_popup_active() =>
        {
            if let Some(area) = app.session_mgr.current_mut().ui.textarea_area {
                if mouse.row >= area.y
                    && mouse.row < area.y + area.height
                    && mouse.column >= area.x
                    && mouse.column < area.x + area.width
                {
                    let session = &app.session_mgr.current();
                    let (row, col) = crate::event::mouse::textarea_mouse_to_cursor(
                        &session.ui.textarea,
                        area,
                        &mouse,
                    );
                    app.session_mgr
                        .current_mut()
                        .ui
                        .textarea
                        .move_cursor(tui_textarea::CursorMove::Jump(row as u16, col as u16));
                    app.session_mgr.current_mut().ui.textarea.start_selection();
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left)
            // Textarea selection extend
            if app.session_mgr.current_mut().ui.textarea.is_selecting() =>
        {
            if let Some(area) = app.session_mgr.current_mut().ui.textarea_area {
                if mouse.row >= area.y && mouse.row < area.y + area.height {
                    let session = &app.session_mgr.current();
                    let (row, col) = crate::event::mouse::textarea_mouse_to_cursor(
                        &session.ui.textarea,
                        area,
                        &mouse,
                    );
                    app.session_mgr
                        .current_mut()
                        .ui
                        .textarea
                        .move_cursor(tui_textarea::CursorMove::Jump(row as u16, col as u16));
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // End scrollbar drag / textarea selection
            app.session_mgr.current_mut().ui.scrollbar_dragging = false;
            app.session_mgr.current_mut().ui.panel_scrollbar_dragging = false;
        }
        _ => {}
    }
}

fn handle_paste_event(app: &mut App, text: &str) {
    // Setup wizard open -- paste into active field
    if let Some(wizard) = &mut app.global_ui.setup_wizard {
        wizard.paste_text(text);
        return;
    }

    // Interaction popup routing
    if app.is_interaction_popup_active() {
        app.paste_to_interaction_popup(text);
        return;
    }

    // Fallback: paste into textarea
    if !app.is_interaction_popup_active() {
        app.session_mgr.current_mut().ui.textarea.insert_str(text);
    }
}

/// Handle an ACP event that arrived through the unified event channel.
///
/// In P1, the AcpNotifier task already converted `AcpNotification` into
/// `TuiEvent::AcpEvent { event, data }`.  Here we reverse that translation
/// back into the legacy `AcpNotification` and delegate to
/// `App::handle_acp_notification`.
///
/// This double-conversion is intentional P1 glue — it avoids rewriting
/// the AcpNotifier task or the App's notification handler.  P2 will
/// eliminate the intermediate JSON round-trip.
fn handle_acp_event(app: &mut App, event_name: &str, data: &serde_json::Value) -> Vec<Effect> {
    use crate::acp_client::AcpNotification;
    use peri_acp::event::AcpEvent;

    let notif = match event_name {
        "agent-event" => {
            // Reconstruct AcpNotification::AgentEvent from the JSON.
            // The AcpNotifier serialized AcpEvent into `data.event`.
            let session_id = data
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let acp_event_value = data.get("event");
            if let Some(ev_value) = acp_event_value {
                match serde_json::from_value::<AcpEvent>(ev_value.clone()) {
                    Ok(acp_event) => AcpNotification::AgentEvent {
                        session_id,
                        event: acp_event,
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to deserialize AcpEvent from AcpEvent JSON");
                        return vec![Effect::Render];
                    }
                }
            } else {
                tracing::warn!("AcpEvent TuiEvent missing 'event' field in data");
                return vec![Effect::Render];
            }
        }
        "session-update" => {
            let session_id = data
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let params = data
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            AcpNotification::SessionUpdate { session_id, params }
        }
        "agent-done" => {
            let session_id = data
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            AcpNotification::AgentDone { session_id }
        }
        "request-permission" => {
            let id = data
                .get("id")
                .and_then(|v| {
                    if v.is_string() {
                        v.as_str()
                            .map(|s| peri_acp::transport::types::RequestId::String(s.to_string()))
                    } else {
                        v.as_i64()
                            .map(peri_acp::transport::types::RequestId::Number)
                    }
                })
                .unwrap_or(peri_acp::transport::types::RequestId::Number(0));
            let params = data
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            AcpNotification::RequestPermission { id, params }
        }
        "elicitation" => {
            let id = data
                .get("id")
                .and_then(|v| {
                    if v.is_string() {
                        v.as_str()
                            .map(|s| peri_acp::transport::types::RequestId::String(s.to_string()))
                    } else {
                        v.as_i64()
                            .map(peri_acp::transport::types::RequestId::Number)
                    }
                })
                .unwrap_or(peri_acp::transport::types::RequestId::Number(0));
            let params = data
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            AcpNotification::Elicitation { id, params }
        }
        "prediction-ready" => {
            let session_id = data
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let text = data
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            AcpNotification::PredictionReady { session_id, text }
        }
        "other" => {
            let msg = data
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            AcpNotification::Other { msg }
        }
        // Peri-mode notifications (method used as event name)
        _ => {
            let session_id = data
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let params = data
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            AcpNotification::Peri {
                session_id,
                method: event_name.to_string(),
                params,
            }
        }
    };

    // Delegate to the legacy App handler.
    let (updated, _should_break, should_return) = app.handle_acp_notification(notif);
    if should_return {
        vec![Effect::Render]
    } else if updated {
        vec![Effect::Render]
    } else {
        vec![Effect::Render]
    }
}

/// Returns `true` if the state machine (idle.rs) already handles this shortcut,
/// so the legacy keyboard handler should be skipped to avoid double-execution.
fn is_sm_handled_shortcut(key: &ratatui::crossterm::event::KeyEvent) -> bool {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    // BackTab: cycle permission mode
    if matches!(key.code, KeyCode::BackTab) {
        return true;
    }

    let ctrl = key.modifiers.intersects(KeyModifiers::CONTROL);
    let shift = key.modifiers.intersects(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Char('t') if ctrl && shift => true, // Ctrl+Shift+T: cycle provider
        KeyCode::Char('t') if ctrl => true,          // Ctrl+T: cycle model
        KeyCode::Char('b') if ctrl => true,          // Ctrl+B: focus bg bar
        KeyCode::Char('o') if ctrl => true,          // Ctrl+O: toggle diff
        _ => false,
    }
}
