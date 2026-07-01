//! Main loop: recv event → state machine (pure) + keyboard fallback → merge effects → loop.
//!
//! The loop drives both the v2 state machine and the keyboard fallback handler:
//!
//! - **1a. State machine** ([`crate::state_machine::handle`]): pure `(State, Event) → (State, Vec<Effect>)`.
//!   Handles shortcuts, mouse, paste, tick, resize, AcpDisconnected, Shutdown.
//! - **1b. Keyboard fallback**: [`crate::event::keyboard`] handles complex UI interactions
//!   (panel dispatch, popups, setup wizard, textarea input, @mention/slash hints, history).
//!   Shortcut ownership is determined by the per-state `owns_shortcut()` functions.
//! - **1c. Merge**: Effects from both paths are merged; `Render` is de-duplicated.
//!
//! The state machine is the authoritative path. Its `State` persists across events;
//! `ViewStore` accumulates view-commits; transitions are pure functions with zero I/O.

use std::collections::HashMap;
use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tracing::debug;

use crate::app::App;
use crate::event::keyboard;
use crate::event::Action;
use crate::panel::read_context::ServiceRegistrySnapshot;
use crate::runtime::apply_context::{ApplyContext, ApplyOutcome};
use crate::runtime::effect::Effect;
use crate::runtime::event_channel::{EventRx, TuiEvent};
use crate::state_machine::input::edit::InputEdit;
use crate::state_machine::state::{PanelReadContext, ShortcutClaim};
use crate::state_machine::transitions::idle::owns_shortcut as idle_owns_shortcut;
use crate::state_machine::transitions::streaming::owns_shortcut as streaming_owns_shortcut;
use crate::state_machine::{
    handle as state_machine_handle, input::sync::to_textarea, transitions::modal, Event as SmEvent,
    IdleState, ModalKind, ModalState, State, StreamingState,
};

/// Target frame interval for loading-spinner animation (~30 FPS).
const TARGET_FRAME_INTERVAL: Duration = Duration::from_millis(33);

// ── Pre-event snapshot ────────────────────────────────────────────────────────

/// Snapshot of UI state captured before the state machine runs.
///
/// Used by `dispatch_fallback` (shortcut dispersal) and by `run()` (tick
/// throttling for render). All fields are cheap `bool` copies — no ViewModel
/// or App references needed.
#[derive(Debug)]
struct PreEventSnapshot {
    is_tick: bool,
    was_idle: bool,
    is_slash_command: bool,
    at_mention_active: bool,
    slash_hint_active: bool,
    popup_active: bool,
    wizard_active: bool,
}

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

    // Initial render before event loop to prevent blank frame on startup.
    ctx.draw_now(app, &mut last_render, &mut state);

    while let Some(event) = rx.recv().await {
        // ── Capture pre-event snapshot ─────────────────────────────────
        let snap = capture_snapshot(&event, &state, app);

        // ── 1a. Drive the pure state machine ───────────────────────────
        let (new_state, sm_effects) = dispatch_sm(&mut state, &event, &snap, app);
        state = new_state;

        // ── 1b. Keyboard fallback + ACP event dispatch ─────────────────
        let fallback_effects = dispatch_fallback(&event, &snap, app, &state);

        // ── 1c. Merge effects (Render de-duplicated) ───────────────────
        let effects = merge_effects(sm_effects, fallback_effects);

        // ── 2. Execute effects ─────────────────────────────────────────
        let (quit, effect_did_mutate_textarea) =
            execute_effects(effects, &mut state, app, ctx).await;
        if quit {
            break;
        }

        // ── 2b. Sync TextArea → state machine InputState ───────────────
        // The keyboard module mutates the TextArea widget directly. When it
        // from_textarea() now only runs for mouse/paste double-write paths.
        // into InputState so SM-owned branches (Enter, Up/Down history) see
        // the latest text. SM-owned state changes (Enter clearing buffer,
        // from_textarea() is conditional on effect_did_mutate_textarea.
        //
        // Cron #37: the same sync must also run when an Effect handler
        // mutates the textarea (Effect::PasteText, MouseTextareaClick,
        // MouseTextareaDrag). Without it, the render-time to_textarea would
        // overwrite the just-applied edit with stale InputState within the
        // same frame — paste would visibly vanish, mouse clicks would snap
        // the cursor back.
        if effect_did_mutate_textarea {
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
            match &mut state {
                State::Idle(idle) => {
                    idle.input.lines = lines;
                    idle.input.cursor = cursor;
                }
                State::Streaming(s) => {
                    s.input.lines = lines;
                    s.input.cursor = cursor;
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
        // Sync state machine InputState → TextArea before rendering.
        match &state {
            State::Idle(idle) => {
                to_textarea(&idle.input, &mut app.session_mgr.current_mut().ui.textarea);
            }
            State::Streaming(s) => {
                to_textarea(&s.input, &mut app.session_mgr.current_mut().ui.textarea);
            }
            _ => {}
        }
        if snap.is_tick {
            let now = std::time::Instant::now();
            if now.duration_since(last_render) >= TARGET_FRAME_INTERVAL {
                ctx.draw_now(app, &mut last_render, &mut state);
            }
        } else {
            ctx.draw_now(app, &mut last_render, &mut state);
        }
    }

    Ok(())
}

// ── Sub-functions ────────────────────────────────────────────────────────────

/// Capture a pre-event snapshot of UI state.
///
/// Must run BEFORE the SM transition consumes `state`, so fields like
/// `was_idle` and `is_slash_command` are available for shortcut dispersal.
fn capture_snapshot(event: &TuiEvent, state: &State, app: &App) -> PreEventSnapshot {
    let is_tick = matches!(event, TuiEvent::Tick);
    let was_idle = matches!(state, State::Idle(_));
    let is_slash_command =
        matches!(state, State::Idle(idle) if idle.input.text().starts_with('/'));
    let (at_mention_active, slash_hint_active) = {
        let ui = &app.session_mgr.current().ui;
        (ui.at_mention.active, ui.slash_hint.active)
    };
    let popup_active = app.is_interaction_popup_active();
    let wizard_active = app.global_ui.setup_wizard.is_some();

    PreEventSnapshot {
        is_tick,
        was_idle,
        is_slash_command,
        at_mention_active,
        slash_hint_active,
        popup_active,
        wizard_active,
    }
}

/// Dispatch an event to the pure state machine.
///
/// Converts `TuiEvent` → `SmEvent`, builds the panel read context + view
/// slice, and dispatches through `state_machine::handle`.  Includes bypass
/// arms for popup/wizard (SM no-op for Key events) and inline overlay
/// (@mention/slash hint, SM no-op for Enter).
fn dispatch_sm(state: &mut State, event: &TuiEvent, snap: &PreEventSnapshot, app: &App) -> (State, Vec<Effect>) {
    let sm_event: SmEvent = event.clone().into();

    // Cron #28: Composite view_slice = state.view + current_turn's
    // incremental VMs.  Include current_turn's VMs in the slice so
    // handle_interrupted sees the FULL picture (committed + streaming).
    let mut view_models: Vec<peri_acp_types::view_model::ViewModel> =
        state.view_models().to_vec();
    if let State::Streaming(s) = &mut *state {
        view_models.extend(s.current_turn.view_models().to_vec());
    }
    let panel_ctx = build_panel_read_context(app, &view_models);

    // Take ownership of the state so we can dispatch.
    let old_state = std::mem::replace(state, State::Idle(IdleState::default()));

    // Cron #25/#38: SM no-op for Key events while popup/wizard is active.
    if (snap.popup_active || snap.wizard_active) && matches!(event, TuiEvent::Key(_)) {
        return (old_state, Vec::new());
    }

    // Cron #37: SM no-op for Enter while inline overlay is active.
    if inline_overlay_active_for_enter(event, snap.at_mention_active, snap.slash_hint_active) {
        return (old_state, Vec::new());
    }

    match old_state {
        State::Modal(modal_state) => {
            let (ns, fx) = modal::handle_with_context(modal_state, sm_event, &panel_ctx);
            (ns, fx)
        }
        other => {
            let (ns, fx) = state_machine_handle(other, sm_event);
            (ns, fx)
        }
    }
}

/// Keyboard fallback + ACP event dispatch.
///
/// Shortcut dispersal (Phase 2.3): uses per-state `owns_shortcut()` instead
/// of the centralized `is_sm_handled_shortcut()`.  Modal owns all keys
/// except Ctrl+C; Idle/Streaming delegate to their respective functions.
fn dispatch_fallback(
    event: &TuiEvent,
    snap: &PreEventSnapshot,
    app: &mut App,
    state: &State,
) -> Vec<Effect> {
    match event {
        TuiEvent::Key(key) => {
            let run_fallback = match state {
                // Modal: SM owns all keys EXCEPT Ctrl+C (keyboard fallback
                // runs `app.interrupt()` for Ctrl+C).
                State::Modal(_) => {
                    let is_ctrl_c = matches!(key.code, KeyCode::Char('c'))
                        && key.modifiers.intersects(KeyModifiers::CONTROL);
                    is_ctrl_c
                }
                // popup / wizard: keyboard fallback owns all keys.
                _ if snap.popup_active || snap.wizard_active => true,
                State::Idle(_) => {
                    let claim = idle_owns_shortcut(
                        key,
                        snap.is_slash_command,
                        snap.at_mention_active,
                        snap.slash_hint_active,
                    );
                    !matches!(claim, ShortcutClaim::SMOwns)
                }
                State::Streaming(_) => {
                    let claim = streaming_owns_shortcut(key);
                    !matches!(claim, ShortcutClaim::SMOwns)
                }
                State::Switching(_) => true,
            };

            if run_fallback {
                match keyboard::handle_key_event(app, *key, state) {
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
            } else {
                vec![Effect::Render]
            }
        }
        TuiEvent::AcpEvent { event, data } => {
            // Phase 2.6 step 7c: pass v2 state.view snapshot to handle_acp_event
            // so handle_interrupted can scan v2 ViewModels.
            //
            // Cron #39: ViewCommit defensive clear of pending_view_rewind_to.
            if event == "view-commit" {
                app.global_ui.pending_view_rewind_to = None;
            }
            handle_acp_event(app, event, data, state.view_models())
        }
        TuiEvent::SessionLoaded { session_id } => {
            debug!(session_id = %session_id, "SessionLoaded event");
            vec![Effect::Render]
        }
        // Mouse: handler for message-area text selection / copy.
        TuiEvent::Mouse(ref mouse) => {
            crate::event::mouse::handle_mouse_event(app, mouse);
            vec![Effect::Render]
        }
        _ => vec![Effect::Render],
    }
}

/// Merge state-machine and fallback effects, deduplicating `Render`.
///
/// Uses `iter().any()` for Render detection (Phase 2.4).
fn merge_effects(sm_effects: Vec<Effect>, fallback_effects: Vec<Effect>) -> Vec<Effect> {
    let mut effects = sm_effects;
    for e in fallback_effects {
        let is_render = matches!(e, Effect::Render);
        if !is_render || !effects.iter().any(|existing| matches!(existing, Effect::Render)) {
            effects.push(e);
        }
    }
    effects
}

/// Execute effects against the live state and app.
///
/// Includes pending_view_rewind_to processing, user bubble draining, and the
/// effect match loop.  Returns `(quit, effect_did_mutate_textarea)`.
async fn execute_effects(
    effects: Vec<Effect>,
    state: &mut State,
    app: &mut App,
    ctx: &mut ApplyContext<'_>,
) -> (bool, bool) {
    let mut quit = false;
    let mut _needs_render = false;
    let mut effect_did_mutate_textarea = false;

    // Cron #34: apply pending_view_rewind_to BEFORE draining pending
    // notes / user bubbles.  Previous order (drain-then-truncate) had a
    // drop bug: handle_interrupted branch 2 enqueues BOTH
    //   - pending_view_rewind_to = Some(user_msg_idx)
    //   - push_system_note("interrupted-resumed") → pending_v2_notes
    // and handle_rewind_completed (Cron #29) does the same with
    // "↩ rewound to message X".  In the old order, the drain appended
    // the note to state.view FIRST, then the truncate dropped it
    // along with the rolled-back messages — user saw the view
    // truncate but the confirmation note vanished.
    //
    // Fixed order: truncate first, then drain.  Notes/bubbles land
    // AFTER the rewind cut and are preserved.
    //
    // 仅对 Idle/Streaming 生效：Modal 保存的是 saved_view（不应被回滚操作触碰），
    // Switching 是过渡态。这两个状态跳过截断。
    if let Some(idx) = app.global_ui.pending_view_rewind_to.take() {
        match state {
            State::Idle(idle) => {
                idle.view.truncate(idx);
                _needs_render = true;
                tracing::debug!(
                    idx,
                    new_len = idle.view.len(),
                    "main_loop: applied pending_view_rewind_to to Idle.view"
                );
            }
            State::Streaming(s) => {
                s.view.truncate(idx);
                _needs_render = true;
                tracing::debug!(
                    idx,
                    new_len = s.view.len(),
                    "main_loop: applied pending_view_rewind_to to Streaming.view"
                );
            }
            State::Modal(_) | State::Switching(_) => {
                tracing::warn!(
                    idx,
                    "pending_view_rewind_to set during Modal/Switching state — ignoring truncate"
                );
            }
        }
    }

    // Cron #24 P1 #2 — drain AskUser 用户回答队列，路由到 v2 state.view。
    {
        let user_bubbles = app
            .session_mgr
            .current_mut()
            .messages
            .drain_pending_v2_user_bubbles();
        if !user_bubbles.is_empty() {
            for text in user_bubbles {
                let (new_state, _) = crate::state_machine::handle(
                    std::mem::replace(state, State::Idle(IdleState::default())),
                    crate::state_machine::event::Event::PushUserBubble(text),
                );
                *state = new_state;
            }
            _needs_render = true;
        }
    }

    for effect in effects {
        match effect {
            Effect::Render => _needs_render = true,
            Effect::ApplyInputOp(ref op) => {
                match state {
                    State::Idle(idle) => {
                        idle.input.apply(op.clone());
                    }
                    State::Streaming(s) => {
                        s.input.apply(op.clone());
                    }
                    _ => {}
                }
                _needs_render = true;
            }
            Effect::DrainPendingNotes => {
                let notes = app
                    .session_mgr
                    .current_mut()
                    .messages
                    .drain_pending_v2_notes();
                if !notes.is_empty() {
                    for note in notes {
                        let (new_state, _) = crate::state_machine::handle(
                            std::mem::replace(state, State::Idle(IdleState::default())),
                            crate::state_machine::event::Event::PushSystemNote(note),
                        );
                        *state = new_state;
                    }
                    _needs_render = true;
                }
            }
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
                    *state = State::Streaming(
                        std::mem::take(idle).into_streaming(),
                    );
                }
                _needs_render = true;
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
                            // Apply to TextArea widget (for rendering + coordinate calc)
                            app.session_mgr.current_mut().ui.textarea.move_cursor(
                                tui_textarea::CursorMove::Jump(r as u16, c as u16),
                            );
                            app.session_mgr.current_mut().ui.textarea.start_selection();
                            // Also apply to v2 InputState for state machine consistency
                            match state {
                                State::Idle(idle) => {
                                    idle.input.start_selection();
                                }
                                State::Streaming(s) => {
                                    s.input.start_selection();
                                }
                                _ => {}
                            }
                            effect_did_mutate_textarea = true;
                        }
                    }
                }
                _needs_render = true;
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
                            // Apply to TextArea widget (for selection tracking)
                            app.session_mgr.current_mut().ui.textarea.move_cursor(
                                tui_textarea::CursorMove::Jump(r as u16, c as u16),
                            );
                            effect_did_mutate_textarea = true;
                        }
                    }
                }
                _needs_render = true;
            }
            Effect::MouseRelease => {
                app.session_mgr.current_mut().ui.scrollbar_dragging = false;
                app.session_mgr.current_mut().ui.panel_scrollbar_dragging = false;
                _needs_render = true;
            }
            // ── Paste routing ───────────────────────────────────
            Effect::PasteText { text } => {
                if let Some(wizard) = &mut app.global_ui.setup_wizard {
                    wizard.paste_text(&text);
                } else if app.is_interaction_popup_active() {
                    app.paste_to_interaction_popup(&text);
                } else {
                    // Write to InputState via state machine (v2 path)
                    match state {
                        State::Idle(idle) => {
                            idle.input.insert_str(&text);
                        }
                        State::Streaming(s) => {
                            s.input.insert_str(&text);
                        }
                        _ => {}
                    }
                    // Also sync to textarea widget so rendering reflects the new content.
                    app.session_mgr.current_mut().ui.textarea.insert_str(&text);
                    effect_did_mutate_textarea = true;
                }
                _needs_render = true;
            }
            // ── App-level effects ─────────────────────────────
            Effect::ShowNotification(text) => {
                // v2 path: route directly through the state machine so the
                // note lands in `state.view` (production render source) on
                // this frame.
                let (new_state, _) = crate::state_machine::handle(
                    std::mem::replace(state, State::Idle(IdleState::default())),
                    crate::state_machine::event::Event::PushSystemNote(text),
                );
                *state = new_state;
                _needs_render = true;
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
                _needs_render = true;
            }
            Effect::SwitchSession(session_id) => {
                tracing::info!(session_id = %session_id, "SwitchSession");
                app.open_thread(session_id);
                *state = State::Switching(crate::state_machine::state::SwitchingState {
                    view: Vec::new(),
                });
                _needs_render = true;
            }
            Effect::OpenPanel(kind) => {
                // Open Modal from Idle OR Streaming. When opened from
                // Streaming, saved_current_turn = Some(...) preserves
                // in-progress streaming data so ClosePanel can restore
                // Streaming instead of dropping the agent's output.
                let panel = crate::panel::registry::create_panel(kind, app);
                match &state {
                    State::Idle(idle) => {
                        *state = State::Modal(ModalState {
                            saved_view: idle.view.clone(),
                            saved_current_turn: None,
                            saved_input: idle.input.clone(),
                            saved_scroll_offset: idle.scroll_offset,
                            saved_history_index: idle.history_index,
                            kind: ModalKind::Panel(panel),
                        });
                        _needs_render = true;
                    }
                    State::Streaming(s) => {
                        *state = State::Modal(ModalState {
                            saved_view: s.view.clone(),
                            saved_current_turn: Some(s.current_turn.clone()),
                            saved_input: s.input.clone(),
                            saved_scroll_offset: s.scroll_offset,
                            saved_history_index: None,
                            kind: ModalKind::Panel(panel),
                        });
                        _needs_render = true;
                    }
                    _ => {}
                }
            }
            Effect::ClosePanel => {
                if let State::Modal(modal) = state {
                    // Restore Idle/Streaming from ModalState.saved_*.
                    let saved_view = std::mem::take(&mut modal.saved_view);
                    let saved_scroll = modal.saved_scroll_offset;
                    let saved_input = std::mem::take(&mut modal.saved_input);
                    let saved_turn = modal.saved_current_turn.take();
                    let saved_history = modal.saved_history_index;

                    let input = if saved_input.text().is_empty() {
                        crate::state_machine::input::InputState::default()
                    } else {
                        saved_input
                    };
                    *state = if let Some(turn) = saved_turn {
                        State::Streaming(StreamingState {
                            current_turn: turn,
                            input,
                            view: saved_view,
                            scroll_offset: saved_scroll,
                        })
                    } else {
                        State::Idle(IdleState {
                            input,
                            scroll_offset: saved_scroll,
                            view: saved_view,
                            history_index: saved_history,
                        })
                    };
                    _needs_render = true;
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
                _needs_render = true;
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
                _needs_render = true;
            }
            Effect::CyclePermissionMode => {
                app.services.permission_mode.cycle();
                app.global_ui.mode_highlight_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                _needs_render = true;
            }
            Effect::FocusBgBar => {
                if !app.session_mgr.current_mut().background_agents.is_empty() {
                    app.session_mgr.current_mut().ui.bg_bar_cursor = Some(0);
                }
                _needs_render = true;
            }
            Effect::ToggleDiff => {
                if app.global_ui.oauth_prompt.is_none() {
                    app.toggle_diff();
                }
                _needs_render = true;
            }
            Effect::PollWorkflow => {
                if app.workflow_polling_active {
                    app.poll_workflow_runs();
                }
            }
            Effect::ClearTextSelection => {
                app.session_mgr.current_mut().ui.text_selection.clear();
            }
            // ── System / Thread / Memory ───────────────────────
            Effect::PushSystemNote(msg) => {
                let (new_state, _) = crate::state_machine::handle(
                    std::mem::replace(state, State::Idle(IdleState::default())),
                    crate::state_machine::event::Event::PushSystemNote(msg),
                );
                *state = new_state;
                _needs_render = true;
            }
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
                _needs_render = true;
            }
            // I/O effects handled by ApplyContext.
            other => match ctx.apply(other, state).await {
                ApplyOutcome::Quit => {
                    quit = true;
                    break;
                }
                ApplyOutcome::Ok => {}
            },
        }
    }

    (quit, effect_did_mutate_textarea)
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

    let (_updated, _should_break, _should_return) = app.handle_acp_notification(notif, view_slice);
    vec![Effect::Render]
}

/// Returns `true` if the event is a plain Enter (no Shift/Alt) AND an inline
/// overlay (@mention popup or slash_completion hint) is active.
///
/// Cron #37: used by main_loop to bypass SM Enter dispatch when these
/// overlays are active. The keyboard fallback owns the injection / hint
/// completion; the SM must not pre-empt it by submitting the raw `@query`
/// or incomplete slash token. See the inline comment at the call site for
/// the full rationale.
fn inline_overlay_active_for_enter(
    event: &TuiEvent,
    at_mention_active: bool,
    slash_hint_active: bool,
) -> bool {
    if !(at_mention_active || slash_hint_active) {
        return false;
    }
    let key = match event {
        TuiEvent::Key(k) => k,
        _ => return false,
    };
    matches!(key.code, KeyCode::Enter)
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
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

    static EMPTY_CACHE: LazyLock<HashMap<String, serde_json::Value>> = LazyLock::new(HashMap::new);

    let session = app.session_mgr.current();

    let services = ServiceRegistrySnapshot::from_app(app);

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machine::input::InputState;
    use crate::state_machine::state::{IdleState, State};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn idle_state() -> State {
        State::Idle(IdleState {
            input: InputState::default(),
            scroll_offset: 0,
            view: vec![],
            history_index: None,
        })
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    // ── merge_effects tests (Phase 2.4) ──────────────────────────────────

    #[test]
    fn test_merge_effects_dedup_render() {
        // 两个路径都产出 Render → 只保留一个
        let sm = vec![Effect::Render];
        let fb = vec![Effect::Render];
        let merged = merge_effects(sm, fb);
        assert_eq!(
            merged.iter().filter(|e| matches!(e, Effect::Render)).count(),
            1,
            "Render must be deduplicated"
        );
    }

    #[test]
    fn test_merge_effects_keeps_non_render() {
        // 非 Render 效果不应被去重
        let sm = vec![Effect::AdvanceSpinner];
        let fb = vec![Effect::Render, Effect::Quit];
        let merged = merge_effects(sm, fb);
        assert!(merged.iter().any(|e| matches!(e, Effect::AdvanceSpinner)));
        assert!(merged.iter().any(|e| matches!(e, Effect::Quit)));
        assert!(merged.iter().any(|e| matches!(e, Effect::Render)));
    }

    #[test]
    fn test_merge_effects_no_sm_render_keeps_fallback_render() {
        // SM 无 Render，fallback 有 Render → 保留
        let sm: Vec<Effect> = vec![];
        let fb = vec![Effect::Render];
        let merged = merge_effects(sm, fb);
        assert_eq!(
            merged.iter().filter(|e| matches!(e, Effect::Render)).count(),
            1
        );
    }

    // ── Cron #25 unified popup-guard regression tests ────────────────────
    //
    // 背景：当 v1 popup 激活（AskUser / HITL / OAuth / Rewind）时，键盘
    // fallback 应独占按键分发。Phase 2.3 后 dispatch_fallback 直接通过
    // owns_shortcut 决策，不再通过 is_sm_handled_shortcut。
    // 此处验证 popup/wizard 时 dispatch_fallback 返回 effects（不跳过）。

    // ── Cron #37: inline overlay (@mention / slash_completion) Enter bypass ──
    //
    // 背景：当 @mention popup 或 slash_completion hint 激活时，用户按 Enter
    // 期望键盘 fallback 注入选中路径 / 完成提示。此前 SM Enter handler 在
    // fallback 之前抢先执行。`is_sm_handled_shortcut` 已对 Enter 返回 false
    // 让 fallback 跑，但那只控制 fallback — 不阻止 SM 自身。
    //
    // 修复：新增 `inline_overlay_active_for_enter` helper，main_loop 用它作为
    // 旁路条件。Enter + (at_mention_active || slash_hint_active) 时 SM no-op，
    // 让 fallback 独占注入选中项。

    #[test]
    fn test_inline_overlay_enter_with_at_mention_active_returns_true() {
        // Enter + at_mention 激活 → true（SM 应旁路）
        let key_event = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            inline_overlay_active_for_enter(&key_event, true, false),
            "Cron #37: Enter + at_mention active must trigger SM bypass"
        );
    }

    #[test]
    fn test_inline_overlay_enter_with_slash_hint_active_returns_true() {
        // Enter + slash_hint 激活 → true（SM 应旁路）
        let key_event = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            inline_overlay_active_for_enter(&key_event, false, true),
            "Cron #37: Enter + slash_hint active must trigger SM bypass"
        );
    }

    #[test]
    fn test_inline_overlay_enter_without_overlay_returns_false() {
        // Enter + 两个 overlay 都不激活 → false（SM 正常处理提交）
        let key_event = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            !inline_overlay_active_for_enter(&key_event, false, false),
            "Cron #37: Enter without overlay must NOT bypass SM (normal submit)"
        );
    }

    #[test]
    fn test_inline_overlay_enter_with_shift_or_alt_returns_false() {
        // Shift+Enter / Alt+Enter 应插入换行而非提交，SM 不需要旁路
        let key_event = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert!(
            !inline_overlay_active_for_enter(&key_event, true, false),
            "Cron #37: Shift+Enter must not trigger bypass (inserts newline)"
        );
        let key_event = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(
            !inline_overlay_active_for_enter(&key_event, true, false),
            "Cron #37: Alt+Enter must not trigger bypass (inserts newline)"
        );
    }

    #[test]
    fn test_inline_overlay_non_enter_key_returns_false() {
        // 非 Enter 按键 + overlay 激活 → false（helper 只关心 Enter）
        let key_event = TuiEvent::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert!(
            !inline_overlay_active_for_enter(&key_event, true, false),
            "Cron #37: Non-Enter keys are not affected by this helper"
        );
    }

    #[test]
    fn test_inline_overlay_non_key_event_returns_false() {
        // Tick / Mouse / Paste 等非 Key 事件 → false（SM 正常处理）
        assert!(
            !inline_overlay_active_for_enter(&TuiEvent::Tick, true, false),
            "Cron #37: Non-Key events are not affected by this helper"
        );
    }

    // ── Cron #27 SwitchSession regression tests ───────────────────────────
    //
    // 背景：Effect::SwitchSession 从 v2 Modal（ThreadBrowser 面板）触发时，
    // 旧代码恢复 `modal.saved_view`（旧会话的 VM 快照），而不是清空让新会话
    // 的 ViewCommit 重新填充。
    //
    // 修复：用 State::Switching 替代 Idle{view: saved_view}。Switching 是
    // 会话切换的标准过渡态，清空 view + 显示 loading + 等待 ViewCommit 落地。
    //
    // 这些测试验证：SwitchSession 后 state 必为 Switching（saved_view 不泄漏）。

    #[test]
    fn test_switch_session_clears_modal_saved_view() {
        // 验证 SwitchSession 的核心契约：Modal.saved_view 不应在新 state 中存活。
        use crate::state_machine::handler::NoopHandler;
        use crate::state_machine::state::{ModalKind, ModalState, SwitchingState};
        use peri_acp_types::view_model::{UserBubbleData, ViewModel as AcpViewModel};

        let old_session_view: Vec<AcpViewModel> = vec![AcpViewModel::UserBubble(UserBubbleData {
            text: "old session message".to_string(),
        })];
        let modal_state = State::Modal(ModalState {
            saved_view: old_session_view,
            saved_current_turn: None,
            saved_input: InputState::default(),
            saved_scroll_offset: 0,
            saved_history_index: None,
            kind: ModalKind::Interaction(Box::new(NoopHandler)),
        });

        let _ = modal_state; // pre-state（不直接消费，文档用途）
        let post_state: State = State::Switching(SwitchingState { view: Vec::new() });

        // 核心断言：post-state 是 Switching，view 为空
        match &post_state {
            State::Switching(s) => {
                assert!(
                    s.view.is_empty(),
                    "Switching state must have empty view — ViewCommit will populate it"
                );
            }
            other => panic!(
                "SwitchSession must transition to State::Switching, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_switching_state_consumes_view_commit_to_idle() {
        // 端到端契约：SwitchSession 进入 Switching → 下一个 ViewCommit 落地
        // → state 变为 Idle 且 view 是新会话的快照（不是旧 saved_view）。
        use crate::state_machine::state::SwitchingState;
        use crate::state_machine::{handle as state_machine_handle, Event};
        use peri_acp_types::event_data::ViewCommit;
        use peri_acp_types::view_model::{UserBubbleData, ViewModel as AcpViewModel};

        let switching = State::Switching(SwitchingState { view: Vec::new() });
        let new_session_view = vec![AcpViewModel::UserBubble(UserBubbleData {
            text: "new session hello".to_string(),
        })];
        let event = Event::AcpEvent(crate::state_machine::AcpEventData::ViewCommit(ViewCommit {
            view_models: new_session_view.clone(),
        }));

        let (next, effects) = state_machine_handle(switching, event);
        match next {
            State::Idle(idle) => {
                assert_eq!(
                    idle.view.len(),
                    1,
                    "Idle view must contain new session's view_models"
                );
                // 验证是新会话的内容，不是旧的 saved_view
                if let AcpViewModel::UserBubble(data) = &idle.view[0] {
                    assert_eq!(
                        data.text, "new session hello",
                        "Idle view must be from ViewCommit, not stale saved_view"
                    );
                } else {
                    panic!("Expected UserBubble, got {:?}", idle.view[0]);
                }
            }
            other => panic!(
                "ViewCommit in Switching must transition to Idle, got {:?}",
                other
            ),
        }
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Render)),
            "ViewCommit in Switching must emit Render"
        );
    }
}
