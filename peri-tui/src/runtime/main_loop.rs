//! Main loop: recv event → state machine (pure) + keyboard fallback → merge effects → loop.
//!
//! The loop drives both the v2 state machine and the keyboard fallback handler:
//!
//! - **1a. State machine** ([`crate::state_machine::handle`]): pure `(State, Event) → (State, Vec<Effect>)`.
//!   Handles shortcuts, mouse, paste, tick, resize, AcpDisconnected, Shutdown.
//! - **1b. Keyboard fallback**: [`crate::event::keyboard`] handles complex UI interactions
//!   (panel dispatch, popups, setup wizard, textarea input, @mention/slash hints, history).
//!   Shortcuts already covered by the state machine are filtered via `is_sm_handled_shortcut()`.
//! - **1c. Merge**: Effects from both paths are merged; `Render` is de-duplicated.
//!
//! The state machine is the authoritative path. Its `State` persists across events;
//! `ViewStore` accumulates view-commits; transitions are pure functions with zero I/O.

use std::collections::HashMap;
use std::time::Duration;

use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tracing::debug;

use crate::app::App;
use crate::event::keyboard;
use crate::event::Action;
use crate::panel::read_context::ServiceRegistrySnapshot;
use crate::runtime::apply_context::{ApplyContext, ApplyOutcome};
use crate::runtime::effect::Effect;
use crate::runtime::event_channel::{EventRx, TuiEvent};
use crate::state_machine::state::PanelReadContext;
use crate::state_machine::{
    handle as state_machine_handle, input::sync::to_textarea, transitions::modal, Event as SmEvent,
    IdleState, ModalKind, ModalState, State, StreamingState,
};

/// Target frame interval for loading-spinner animation (~30 FPS).
const TARGET_FRAME_INTERVAL: Duration = Duration::from_millis(33);

// ── Public entry point ──────────────────────────────────────────────────────

/// Run the v2 main loop until the channel closes or an effect requests Quit.
///
/// The loop is the **only** place that reads from the event channel and the
/// **only** place that performs I/O (terminal draw, ACP send, clipboard).
///
/// Uses the dual-path architecture: state machine (1a) + keyboard fallback (1b),
/// with effects merged and Render de-duplicated.
pub async fn run(mut rx: EventRx, ctx: &mut ApplyContext<'_>, app: &mut App) -> anyhow::Result<()> {
    let mut last_render = std::time::Instant::now();

    // v2 state machine state. Persists across events. Initial = Idle.
    let mut state: State = State::Idle(IdleState::default());

    while let Some(event) = rx.recv().await {
        let is_tick = matches!(event, TuiEvent::Tick);

        // ── Pre-event snapshot: textarea text length, used by 2b sync to
        // clear InputState.prediction when the keyboard fallback shrunk
        // the buffer (Backspace/Ctrl+U/Ctrl+W). Captured before any event
        // processing mutates the widget.
        let old_text_len: usize = app
            .session_mgr
            .current()
            .ui
            .textarea
            .lines()
            .iter()
            .map(|l| l.chars().count())
            .sum();

        // ── 1a. Drive the pure state machine ────────────────────────────
        // Convert TuiEvent → SmEvent (decode ACP {event, data} into typed
        // AcpEventData variants) and dispatch to the transition function.
        // When state is Modal, use handle_with_context() with real App data
        // so panels have access to i18n, scroll offset, panel area, etc.
        // ── Pre-transition snapshots for Enter guard ──────────────────
        // Must be captured BEFORE the SM transition consumes `state`, so
        // `is_sm_handled_shortcut` can still filter Enter when the SM
        // transitions Idle → Streaming in the same frame.
        let was_idle = matches!(&state, State::Idle(_));
        // Slash commands (e.g. /history, /model) must go through the
        // keyboard fallback for CommandRegistry::dispatch.  Capture this
        // before the SM clears the input buffer on Enter.
        let is_slash_command =
            matches!(&state, State::Idle(idle) if idle.input.text().starts_with('/'));

        let sm_event: SmEvent = event.clone().into();
        let new_state: State;
        let sm_effects: Vec<Effect>;
        let view_models: Vec<peri_acp_types::view_model::ViewModel> = state.view_models().to_vec();
        let panel_ctx = build_panel_read_context(app, &view_models);
        match state {
            State::Modal(modal_state) => {
                let (ns, fx) = modal::handle_with_context(modal_state, sm_event, &panel_ctx);
                new_state = ns;
                sm_effects = fx;
            }
            _ => {
                let (ns, fx) = state_machine_handle(state, sm_event);
                new_state = ns;
                sm_effects = fx;
            }
        }
        state = new_state;

        // ── 1b. Keyboard fallback + ACP event dispatch ─────────────────
        // These are the only remaining paths not yet handled by the pure
        // state machine. Keyboard dispatch is filtered to avoid double-
        // executing shortcuts the state machine already owns.
        //
        // Track whether keyboard fallback actually ran for this event.
        // The 2b sync (TextArea → InputState) runs ONLY when keyboard
        // fallback executed — otherwise it would overwrite SM-owned
        // state changes (e.g. SM clearing InputState on Enter) with the
        // stale TextArea snapshot. See idle.rs module doc for the full
        // rationale on textarea being the single source of truth.
        let mut keyboard_did_run = false;
        let fallback_effects = match &event {
            TuiEvent::Key(key)
                if !is_sm_handled_shortcut(key, &state, was_idle, is_slash_command) =>
            {
                keyboard_did_run = true;
                match keyboard::handle_key_event(app, *key) {
                    Ok(Some(Action::Quit)) => vec![Effect::Quit],
                    Ok(Some(Action::Submit(input))) => {
                        app.submit_message(input);
                        vec![Effect::Render]
                    }
                    Ok(Some(Action::Effects(effects))) => effects,
                    Ok(Some(Action::Redraw)) | Ok(None) => vec![Effect::Render],
                    Err(e) => {
                        tracing::warn!(error = %e, "keyboard handler error");
                        vec![Effect::Render]
                    }
                }
            }
            TuiEvent::AcpEvent { event, data } => {
                // Phase 2.6 step 7c: pass v2 state.view snapshot to handle_acp_event
                // so handle_interrupted can scan v2 ViewModels (not v1 view_messages).
                // Captured AFTER the SM transition above (state.view reflects current).
                handle_acp_event(app, event, data, state.view_models())
            }
            TuiEvent::SessionLoaded { session_id } => {
                debug!(session_id = %session_id, "SessionLoaded event");
                vec![Effect::Render]
            }
            // ── Mouse: handler for message-area text selection / copy ─
            // The state machine handles textarea mouse interaction (click/drag
            // mapped to MouseTextareaClick/Drag/Release effects).  Message-area
            // text selection, scrollbar drag, and clipboard copy live in the
            // mouse handler which is NOT covered by the SM.  Call it here
            // to prevent the copy feature from being silently dropped.
            TuiEvent::Mouse(ref mouse) => {
                crate::event::mouse::handle_mouse_event(app, mouse);
                vec![Effect::Render]
            }
            _ => vec![Effect::Render],
        };

        // ── 1c. Merge effects (Render de-duplicated) ────────────────────
        let mut effects: Vec<Effect> = sm_effects;
        for e in fallback_effects {
            if !effects.contains(&e) {
                effects.push(e);
            }
        }

        // Phase 2.4 — drain pending v2 notes pushed by App-method paths
        // (thread_ops, agent_ops, rewind, polling, etc.) that don't return
        // Vec<Effect>. These notes were pushed to view_messages (v1 cache)
        // AND queued in pending_v2_notes; we route them through the state
        // machine here so they reach state.view (production render source).
        // We do NOT emit Effect::PushSystemNote here — that handler would
        // re-push to view_messages (already done by the original call),
        // causing duplicate notes. The state-machine route alone is enough
        // to keep state.view in sync.
        //
        // (Drain happens after `needs_render` is declared below.)

        // ── 2. Execute effects ─────────────────────────────────────────
        let mut quit = false;
        let mut needs_render = false;

        // Phase 2.4 — drain (see note above).
        {
            let notes = app
                .session_mgr
                .current_mut()
                .messages
                .drain_pending_v2_notes();
            if !notes.is_empty() {
                for note in notes {
                    let (new_state, _) = crate::state_machine::handle(
                        state,
                        crate::state_machine::event::Event::PushSystemNote(note),
                    );
                    state = new_state;
                }
                needs_render = true;
            }
        }

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
                    // Transition Idle → Streaming so that incoming
                    // TextChunk / ReasoningChunk / ToolStarted events
                    // accumulate in current_turn instead of being dropped.
                    if let State::Idle(idle) = state {
                        state = State::Streaming(idle.into_streaming());
                    }
                    needs_render = true;
                }
                Effect::PollAgent => {
                    // Phase 2.6 step 7c: pass v2 view snapshot so any interrupt
                    // arriving during poll_agent's drain loop is handled correctly.
                    let view_snapshot: Vec<peri_acp_types::view_model::ViewModel> =
                        state.view_models().to_vec();
                    app.poll_agent(&view_snapshot);
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
                // ── Mouse textarea interaction ───────────────────────────
                Effect::MouseTextareaClick { row, column } => {
                    if !app.is_interaction_popup_active() {
                        if let Some(area) = app.session_mgr.current_mut().ui.textarea_area {
                            if row >= area.y
                                && row < area.y + area.height
                                && column >= area.x
                                && column < area.x + area.width
                            {
                                let (r, c) = crate::event::mouse::textarea_mouse_to_cursor(
                                    &app.session_mgr.current().ui.textarea,
                                    area,
                                    &MouseEvent {
                                        kind: MouseEventKind::Down(MouseButton::Left),
                                        column,
                                        row,
                                        modifiers: KeyModifiers::NONE,
                                    },
                                );
                                app.session_mgr.current_mut().ui.textarea.move_cursor(
                                    tui_textarea::CursorMove::Jump(r as u16, c as u16),
                                );
                                app.session_mgr.current_mut().ui.textarea.start_selection();
                            }
                        }
                    }
                    needs_render = true;
                }
                Effect::MouseTextareaDrag { row, column } => {
                    if app.session_mgr.current_mut().ui.textarea.is_selecting() {
                        if let Some(area) = app.session_mgr.current_mut().ui.textarea_area {
                            if row >= area.y && row < area.y + area.height {
                                let (r, c) = crate::event::mouse::textarea_mouse_to_cursor(
                                    &app.session_mgr.current().ui.textarea,
                                    area,
                                    &MouseEvent {
                                        kind: MouseEventKind::Drag(MouseButton::Left),
                                        column,
                                        row,
                                        modifiers: KeyModifiers::NONE,
                                    },
                                );
                                app.session_mgr.current_mut().ui.textarea.move_cursor(
                                    tui_textarea::CursorMove::Jump(r as u16, c as u16),
                                );
                            }
                        }
                    }
                    needs_render = true;
                }
                Effect::MouseRelease => {
                    app.session_mgr.current_mut().ui.scrollbar_dragging = false;
                    app.session_mgr.current_mut().ui.panel_scrollbar_dragging = false;
                    needs_render = true;
                }
                // ── Paste routing ───────────────────────────────────
                Effect::PasteText { text } => {
                    if let Some(wizard) = &mut app.global_ui.setup_wizard {
                        wizard.paste_text(&text);
                    } else if app.is_interaction_popup_active() {
                        app.paste_to_interaction_popup(&text);
                    } else {
                        app.session_mgr.current_mut().ui.textarea.insert_str(&text);
                    }
                    needs_render = true;
                }
                // ── Agent control ─────────────────────────────────
                // (InterruptAgent / ClearPendingMessages removed — legacy
                // keyboard handles Ctrl+C / Esc-during-loading directly.)
                // ── App-level effects (P3 Integration) ─────────────
                Effect::ShowNotification(text) => {
                    // v1 path: legacy view_messages (will be removed in Phase 2.6).
                    app.push_system_note(text.clone());
                    // v2 path: route through state machine so state.view is
                    // the single source of truth (Phase 2.4).
                    let (new_state, _) = crate::state_machine::handle(
                        state,
                        crate::state_machine::event::Event::PushSystemNote(text),
                    );
                    state = new_state;
                    needs_render = true;
                }
                Effect::UpdateConfig { key, value } => {
                    let cfg_arc = app.services.peri_config.clone();
                    let mut cfg = cfg_arc.write();
                    let parts: Vec<&str> = key.splitn(3, '.').collect();
                    match parts.as_slice() {
                        ["active_provider_id"] => {
                            cfg.config.active_provider_id = value.clone();
                        }
                        ["provider", id, field] => {
                            let provider = cfg.config.providers.iter_mut().find(|p| p.id == *id);
                            if let Some(p) = provider {
                                match *field {
                                    "name" => p.name = Some(value.clone()),
                                    "type" => p.provider_type = value.clone(),
                                    "base_url" => p.base_url = value.clone(),
                                    "api_key" => p.api_key = value.clone(),
                                    "opus_model" => p.models.opus = value.clone(),
                                    "sonnet_model" => p.models.sonnet = value.clone(),
                                    "haiku_model" => p.models.haiku = value.clone(),
                                    _ => {}
                                }
                            } else if !value.is_empty() {
                                let mut new_provider = peri_acp::provider::config::ProviderConfig {
                                    id: id.to_string(),
                                    ..Default::default()
                                };
                                match *field {
                                    "name" => new_provider.name = Some(value.clone()),
                                    "type" => new_provider.provider_type = value.clone(),
                                    "base_url" => new_provider.base_url = value.clone(),
                                    "api_key" => new_provider.api_key = value.clone(),
                                    "opus_model" => new_provider.models.opus = value.clone(),
                                    "sonnet_model" => new_provider.models.sonnet = value.clone(),
                                    "haiku_model" => new_provider.models.haiku = value.clone(),
                                    _ => {}
                                }
                                cfg.config.providers.push(new_provider);
                            }
                        }
                        _ => {
                            tracing::warn!(
                                key = %key,
                                value = %value,
                                "UpdateConfig: unknown key path"
                            );
                        }
                    }
                    let _ = App::save_config(&cfg, app.services.config_path_override.as_deref());
                    drop(cfg);
                    if let Some(ref acp_client) = app.acp_client {
                        let acp = acp_client.clone();
                        let k = key.clone();
                        let v = value.clone();
                        tokio::spawn(async move {
                            let _ = acp.set_config_option(&k, &v).await;
                        });
                    }
                    needs_render = true;
                }
                Effect::SwitchSession(session_id) => {
                    tracing::info!(session_id = %session_id, "SwitchSession");
                    app.open_thread(session_id);
                    // Close the panel so the user sees the loaded thread.
                    // SwitchSession always lands in Idle — the new session
                    // starts fresh even if we were Streaming when the modal
                    // opened, so saved_current_turn is intentionally dropped.
                    if let State::Modal(modal) = state {
                        let input = if modal.saved_input.text().is_empty() {
                            crate::state_machine::input::sync::from_textarea(
                                &app.session_mgr.current().ui.textarea,
                            )
                        } else {
                            modal.saved_input
                        };
                        let idle = IdleState {
                            input,
                            scroll_offset: modal.saved_scroll_offset,
                            view: modal.saved_view,
                            double_esc_timer: modal.saved_double_esc_timer,
                            history_index: modal.saved_history_index,
                        };
                        state = State::Idle(idle);
                    }
                    needs_render = true;
                }
                Effect::OpenPanel(kind) => {
                    // Open Modal from Idle OR Streaming. When opened from
                    // Streaming, saved_current_turn = Some(...) preserves
                    // in-progress streaming data so ClosePanel can restore
                    // Streaming instead of dropping the agent's output.
                    let panel = crate::panel::registry::create_panel(kind, app);
                    if let State::Idle(idle) = state {
                        state = State::Modal(ModalState {
                            saved_view: idle.view,
                            saved_current_turn: None,
                            saved_input: idle.input,
                            saved_scroll_offset: idle.scroll_offset,
                            saved_history_index: idle.history_index,
                            saved_double_esc_timer: idle.double_esc_timer,
                            kind: ModalKind::Panel(panel),
                        });
                        needs_render = true;
                    } else if let State::Streaming(s) = state {
                        state = State::Modal(ModalState {
                            saved_view: s.view,
                            saved_current_turn: Some(s.current_turn),
                            saved_input: s.input,
                            saved_scroll_offset: s.scroll_offset,
                            saved_history_index: None,
                            saved_double_esc_timer: None,
                            kind: ModalKind::Panel(panel),
                        });
                        needs_render = true;
                    }
                }
                Effect::ClosePanel => {
                    if let State::Modal(modal) = state {
                        // Restore Idle/Streaming from ModalState.saved_*.
                        // Falls back to TextArea snapshot when saved_input is
                        // empty (panel opened via legacy fallback path).
                        let input = if modal.saved_input.text().is_empty() {
                            crate::state_machine::input::sync::from_textarea(
                                &app.session_mgr.current().ui.textarea,
                            )
                        } else {
                            modal.saved_input
                        };
                        state = if let Some(turn) = modal.saved_current_turn {
                            // Modal was opened from Streaming — restore it so
                            // accumulated streaming progress is preserved.
                            State::Streaming(StreamingState {
                                current_turn: turn,
                                input,
                                view: modal.saved_view,
                                scroll_offset: modal.saved_scroll_offset,
                            })
                        } else {
                            State::Idle(IdleState {
                                input,
                                scroll_offset: modal.saved_scroll_offset,
                                view: modal.saved_view,
                                double_esc_timer: modal.saved_double_esc_timer,
                                history_index: modal.saved_history_index,
                            })
                        };
                        needs_render = true;
                    }
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
                        let session = app.session_mgr.current_mut();
                        session.messages.push_system_note(app.services.lc.tr_args(
                            "config-save-failed",
                            &[("error".into(), e.to_string().into())],
                        ));
                        session.messages.message_cache = None;
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
                            let session = app.session_mgr.current_mut();
                            session.messages.push_system_note(app.services.lc.tr_args(
                                "config-save-failed",
                                &[("error".into(), e.to_string().into())],
                            ));
                            session.messages.message_cache = None;
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
                // (OpenRewindPrompt removed — legacy keyboard handles Esc.)
                // ── System / Thread / Memory ───────────────────────
                Effect::PushSystemNote(msg) => {
                    // v1 path: legacy view_messages (will be removed in Phase 2.6).
                    app.push_system_note(msg.clone());
                    // v2 path: route through state machine so state.view is
                    // the single source of truth (Phase 2.4).
                    let (new_state, _) = crate::state_machine::handle(
                        state,
                        crate::state_machine::event::Event::PushSystemNote(msg),
                    );
                    state = new_state;
                    needs_render = true;
                }
                // (OpenThreadWithFeedback removed — no SM emitter; thread
                // browser panel will emit its own PanelEffect when wired.)
                Effect::MemoryPanelOpenEditor { ref path } => {
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| {
                        if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
                            "vim".to_string()
                        } else {
                            "notepad".to_string()
                        }
                    });
                    let path_clone = path.clone();
                    tokio::task::spawn_blocking(move || {
                        use std::process::Command;
                        let result = Command::new(&editor).arg(&path_clone).spawn();
                        match result {
                            Ok(mut child) => {
                                let _ = child.wait();
                            }
                            Err(e) => {
                                tracing::warn!(
                                    editor = %editor,
                                    path = %path_clone.display(),
                                    error = %e,
                                    "MemoryPanelOpenEditor: failed to spawn editor"
                                );
                                // Fallback: try nano
                                if editor != "nano" {
                                    if let Ok(mut child) =
                                        Command::new("nano").arg(&path_clone).spawn()
                                    {
                                        let _ = child.wait();
                                    }
                                }
                            }
                        }
                    });
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

        // ── 2b. Sync TextArea → state machine InputState ───────────────
        // The keyboard module mutates the TextArea widget directly. When it
        // runs (keyboard_did_run=true), pull the widget's lines+cursor back
        // into InputState so SM-owned branches (Enter, Up/Down history) see
        // the latest text. SM-owned state changes (Enter clearing buffer,
        // history navigation) are NOT overwritten because keyboard_did_run
        // is false for those events (is_sm_handled_shortcut filters them).
        //
        // Prediction clearing: previously attached to SM's Backspace/Ctrl+U/
        // Ctrl+W arms (now deleted). Reimplemented here by comparing text
        // length before/after — any shrink clears prediction. This catches
        // all edit paths that reduce text, regardless of which key triggered
        // it (Backspace, Ctrl+W word-delete, Ctrl+U line-delete, etc.).
        //
        // Semantic fields (at_mention, slash_completion, history, attachments)
        // remain managed independently by the state machine — only lines +
        // cursor are synced, plus the prediction side-effect on shrink.
        if keyboard_did_run {
            let ta = &app.session_mgr.current().ui.textarea;
            let lines: Vec<String> = ta.lines().to_vec();
            let (row, col_char) = ta.cursor();
            // tui_textarea::cursor() returns a CHARACTER index, but
            // CursorPos.col_byte is a BYTE offset. Convert here.
            let col_byte: usize = lines
                .get(row)
                .map(|line| line.chars().take(col_char).map(|c| c.len_utf8()).sum())
                .unwrap_or(0);
            let cursor = crate::state_machine::input::CursorPos::new(row, col_byte);
            let new_text_len: usize = lines.iter().map(|l| l.chars().count()).sum();
            let text_shrunk = new_text_len < old_text_len;
            match &mut state {
                State::Idle(idle) => {
                    idle.input.lines = lines;
                    idle.input.cursor = cursor;
                    if text_shrunk {
                        idle.input.prediction = None;
                    }
                }
                State::Streaming(s) => {
                    s.input.lines = lines;
                    s.input.cursor = cursor;
                    if text_shrunk {
                        s.input.prediction = None;
                    }
                }
                _ => {}
            }
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
            // Sync state machine InputState → TextArea before rendering,
            // so that state-machine-originated changes (history restore,
            // rewind, prediction) are reflected in the widget.
            // Sync input state from state machine to legacy textarea.
            match &state {
                State::Idle(idle) => {
                    to_textarea(&idle.input, &mut app.session_mgr.current_mut().ui.textarea);
                }
                State::Streaming(s) => {
                    to_textarea(&s.input, &mut app.session_mgr.current_mut().ui.textarea);
                }
                _ => {}
            }
            if is_tick {
                let now = std::time::Instant::now();
                if now.duration_since(last_render) >= TARGET_FRAME_INTERVAL {
                    ctx.draw_now(app, &mut last_render, &mut state);
                }
            } else {
                ctx.draw_now(app, &mut last_render, &mut state);
            }
        }
    }

    Ok(())
}

// ── ACP event bridge ──────────────────────────────────────────────────────────

/// Handle an ACP event that arrived through the unified event channel.
///
/// The AcpNotifier task converts `AcpNotification` into
/// `TuiEvent::AcpEvent { event, data }`.  Here we reverse that translation
/// back into `AcpNotification` and delegate to
/// `App::handle_acp_notification`.
fn handle_acp_event(
    app: &mut App,
    event_name: &str,
    data: &serde_json::Value,
    view_slice: &[peri_acp_types::view_model::ViewModel],
) -> Vec<Effect> {
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

    // Delegate to the App handler.
    let (updated, _should_break, should_return) = app.handle_acp_notification(notif, view_slice);
    if should_return {
        vec![Effect::Render]
    } else if updated {
        vec![Effect::Render]
    } else {
        vec![Effect::Render]
    }
}

/// Returns `true` if the state machine already handles this shortcut,
/// so the keyboard fallback handler should be skipped to avoid double-execution.
///
/// When the state machine is in [`State::Modal`], ALL keys are intercepted —
/// the state machine dispatches every key to the active v2 panel/handler,
/// and the keyboard fallback handler must not also process them.
fn is_sm_handled_shortcut(
    key: &ratatui::crossterm::event::KeyEvent,
    state: &State,
    was_idle: bool,
    is_slash_command: bool,
) -> bool {
    // Modal: state machine handles EVERY key (dispatches to panel/handler).
    if matches!(state, State::Modal(_)) {
        return true;
    }

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    // BackTab: cycle permission mode
    if matches!(key.code, KeyCode::BackTab) {
        return true;
    }

    // Enter (no Shift/Alt): state machine handles submission,
    // EXCEPT for slash commands — those must go through the keyboard
    // fallback so CommandRegistry::dispatch can route them.
    // Use pre-transition flags: is_slash_command was captured before the
    // SM transition consumed the state (Idle → Streaming on Enter).
    if matches!(key.code, KeyCode::Enter)
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
        && (was_idle || matches!(state, State::Idle(_)))
    {
        if is_slash_command {
            return false;
        }
        return true;
    }

    let ctrl = key.modifiers.intersects(KeyModifiers::CONTROL);
    let shift = key.modifiers.intersects(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Char('t') if ctrl && shift => true, // Ctrl+Shift+T: cycle provider
        KeyCode::Char('t') if ctrl => true,          // Ctrl+T: cycle model
        KeyCode::Char('b') if ctrl => true,          // Ctrl+B: focus bg bar
        KeyCode::Char('o') if ctrl => true,          // Ctrl+O: toggle diff
        KeyCode::Char('p') if ctrl => true,          // Ctrl+P: open Model panel
        _ => false,
    }
}

/// Build a [`PanelReadContext`] from live App data.
///
/// Called once per event when the state machine is in [`State::Modal`],
/// giving v2 panels access to i18n, scroll offset, panel area, etc.
/// The returned context borrows from `app` — it is consumed within the
/// same event iteration and never escapes the main loop tick.
fn build_panel_read_context<'a>(
    app: &'a App,
    view_models: &'a [peri_acp_types::view_model::ViewModel],
) -> PanelReadContext<'a> {
    use std::sync::LazyLock;

    // Thread-local for ServiceRegistrySnapshot (same pattern as apply_context.rs).
    std::thread_local! {
        static SNAPSHOT: std::cell::RefCell<ServiceRegistrySnapshot> =
            std::cell::RefCell::new(ServiceRegistrySnapshot::new());
    }
    static EMPTY_CACHE: LazyLock<HashMap<String, serde_json::Value>> = LazyLock::new(HashMap::new);

    let session = app.session_mgr.current();

    let services: &ServiceRegistrySnapshot = SNAPSHOT.with(|cell| {
        *cell.borrow_mut() = ServiceRegistrySnapshot::from_app(app);
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
