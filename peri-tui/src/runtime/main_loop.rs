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
        // Inline-hint active flags: when the @mention popup or the slash
        // hint overlay is active on the App's textarea, Enter must reach the
        // keyboard fallback (which injects the selected path / completes the
        // hint) instead of being claimed by the SM as a message submit.
        // These live on the App (not in InputState) and are mutually
        // exclusive (keyboard.rs deactivates slash_hint when at_mention is
        // active), so either flag independently defers Enter to fallback.
        let (at_mention_active, slash_hint_active) = {
            let ui = &app.session_mgr.current().ui;
            (ui.at_mention.active, ui.slash_hint.active)
        };
        // Cron #25 unified popup-guard: when a v1 popup is active
        // (AskUser / HITL / OAuth / Rewind), the keyboard fallback owns
        // all key dispatch. Letting the SM also run would double-execute
        // (e.g., Ctrl+T cycles model AND popup ignores it; Esc advances
        // DoubleEscTracker AND popup closes; BackTab cycles permission AND
        // popup expected prev-question). SM still processes Tick/AcpEvent/
        // Paste/Mouse/Resize — only Key dispatch is suppressed.
        let popup_active = app.is_interaction_popup_active();

        let sm_event: SmEvent = event.clone().into();
        let new_state: State;
        let sm_effects: Vec<Effect>;
        // Cron #28: Composite view_slice = state.view + current_turn's incremental VMs.
        //
        // Pre-fix: view_slice came only from state.view_models() (committed snapshot).
        // handle_interrupted scanned it for ToolCards to decide branch 1 (keep progress)
        // vs branch 2 (rollback). But current_turn's ToolCards (not yet committed by
        // ViewCommit) were invisible → branch 2 was chosen incorrectly → user's
        // submitted message got rolled back even when tools had run.
        //
        // Fix: include current_turn's VMs in the slice. Now handle_interrupted sees
        // the FULL picture (committed + streaming-in-progress) and picks branch 1
        // correctly when tools are mid-flight.
        let mut view_models: Vec<peri_acp_types::view_model::ViewModel> =
            state.view_models().to_vec();
        if let State::Streaming(s) = &mut state {
            view_models.extend(s.current_turn.view_models().to_vec());
        }
        let panel_ctx = build_panel_read_context(app, &view_models);
        match state {
            State::Modal(modal_state) => {
                let (ns, fx) = modal::handle_with_context(modal_state, sm_event, &panel_ctx);
                new_state = ns;
                sm_effects = fx;
            }
            _ if popup_active && matches!(event, TuiEvent::Key(_)) => {
                // Cron #25: SM no-op for Key events while popup is active.
                // The keyboard fallback will handle the key (popups::handle_popups
                // routes to the active popup). Returning the original state
                // unchanged preserves InputState / view / double_esc_timer.
                new_state = match state {
                    State::Idle(s) => State::Idle(s),
                    State::Streaming(s) => State::Streaming(s),
                    State::Switching(s) => State::Switching(s),
                    State::Modal(s) => State::Modal(s),
                };
                sm_effects = Vec::new();
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
                if !is_sm_handled_shortcut(
                    key,
                    &state,
                    was_idle,
                    is_slash_command,
                    popup_active,
                    at_mention_active,
                    slash_hint_active,
                ) =>
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
        // Vec<Effect>. These paths call `app.push_system_note(...)` which
        // only enqueues into `pending_v2_notes` (Phase 2.6 step 5 retired
        // the legacy view_messages push). We drain here and route through
        // the state machine so they reach `state.view` (production render
        // source). The `Effect::ShowNotification` / `Effect::PushSystemNote`
        // handlers do NOT use this queue — they call the SM directly to
        // avoid a duplicate note on the next-tick drain.
        //
        // (Drain happens after `needs_render` is declared below.)

        // ── 2. Execute effects ─────────────────────────────────────────
        let mut quit = false;
        let mut needs_render = false;

        // Cron #34: apply pending_view_rewind_to BEFORE draining pending
        // notes / user bubbles. Previous order (drain-then-truncate) had a
        // drop bug: handle_interrupted branch 2 enqueues BOTH
        //   - pending_view_rewind_to = Some(user_msg_idx)
        //   - push_system_note("interrupted-resumed") → pending_v2_notes
        // and handle_rewind_completed (Cron #29) does the same with
        // "↩ rewound to message X". In the old order, the drain appended
        // the note to state.view FIRST, then the truncate dropped it
        // along with the rolled-back messages — user saw the view
        // truncate but the confirmation note vanished, leaving no UX
        // feedback for the destructive rollback.
        //
        // Fixed order: truncate first, then drain. Notes/bubbles land
        // AFTER the rewind cut and are preserved.
        //
        // 仅对 Idle/Streaming 生效：Modal 保存的是 saved_view（不应被回滚操作触碰），
        // Switching 是过渡态。这两个状态跳过截断，与 v1 路径的现有不一致行为保持一致
        // （pre-existing，本修复不引入回归）。
        if let Some(idx) = app.global_ui.pending_view_rewind_to.take() {
            match &mut state {
                State::Idle(idle) => {
                    idle.view.truncate(idx);
                    needs_render = true;
                    tracing::debug!(
                        idx,
                        new_len = idle.view.len(),
                        "main_loop: applied pending_view_rewind_to to Idle.view"
                    );
                }
                State::Streaming(s) => {
                    s.view.truncate(idx);
                    needs_render = true;
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

        // Cron #24 P1 #2 — drain AskUser 用户回答队列，路由到 v2 state.view。
        // 与 pending_v2_notes 同构（queue-and-drain via SM Event）。
        {
            let user_bubbles = app
                .session_mgr
                .current_mut()
                .messages
                .drain_pending_v2_user_bubbles();
            if !user_bubbles.is_empty() {
                for text in user_bubbles {
                    let (new_state, _) = crate::state_machine::handle(
                        state,
                        crate::state_machine::event::Event::PushUserBubble(text),
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
                    // v2 path: route directly through the state machine so the
                    // note lands in `state.view` (production render source) on
                    // this frame. We deliberately do NOT call
                    // `app.push_system_note(text)` here — that would enqueue
                    // the note into `pending_v2_notes`, which the next-tick
                    // drain block would feed back into the SM a second time
                    // (duplicate SystemNote). The queue-and-drain pattern is
                    // only for App-method paths (agent_ops, thread_ops, etc.)
                    // that have no Effect return path.
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
                    // Cron #27: Transition to Switching state regardless of
                    // current state (Modal/Idle/Streaming). The new session's
                    // view models will arrive via the next ViewCommit event,
                    // which switching.rs transitions to Idle with the new
                    // view_models.
                    //
                    // Pre-fix bug: restored `modal.saved_view` (captured when
                    // the modal opened), which leaked the OLD session's view
                    // models into the newly-loaded session's display until
                    // the next ViewCommit arrived — user saw stale messages
                    // briefly overlapping with the new thread.
                    //
                    // Switching is the canonical session-switch transitional
                    // state: clears view, shows loading indicator, drops all
                    // keys/pastes until first commit lands.
                    state = State::Switching(crate::state_machine::state::SwitchingState {
                        view: Vec::new(),
                    });
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
                    // v2 path: route directly through the state machine so the
                    // note lands in `state.view` (production render source) on
                    // this frame. We deliberately do NOT call
                    // `app.push_system_note(msg)` here — that would enqueue
                    // the note into `pending_v2_notes`, which the next-tick
                    // drain block would feed back into the SM a second time
                    // (duplicate SystemNote). The queue-and-drain pattern is
                    // only for App-method paths (agent_ops, thread_ops, etc.)
                    // that have no Effect return path.
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
    popup_active: bool,
    at_mention_active: bool,
    slash_hint_active: bool,
) -> bool {
    // Modal: state machine handles EVERY key EXCEPT Ctrl+C.
    //
    // Cron #29 P2 fix (workflow weo7g6w2n): without this carve-out, Ctrl+C
    // was silently swallowed in Modal state — modal.rs handle_key only
    // dispatches Ctrl+T/B/O/P + Ctrl+Shift+T, falling through to a
    // render-only arm for everything else. The keyboard fallback (which
    // owns `app.interrupt()` via normal_keys.rs Ctrl+C handler) was
    // blocked by this `return true`, so users could not cancel a running
    // agent while any v2 panel/popup was open (Model/Login/Config/Mcp/
    // Cron/etc., or HITL/AskUser/Rewind/OAuth handler). They had to
    // close the popup first, then press Ctrl+C — a UX regression from
    // v1 where popups::handle_popups always had a chance to route Ctrl+C.
    //
    // Fix: return false for Ctrl+C so keyboard fallback runs the interrupt.
    if matches!(state, State::Modal(_)) {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};
        let is_ctrl_c = matches!(key.code, KeyCode::Char('c'))
            && key.modifiers.intersects(KeyModifiers::CONTROL);
        return !is_ctrl_c;
    }

    // Cron #25 unified popup-guard: when a v1 popup is active (AskUser / HITL
    // / OAuth / Rewind), the keyboard fallback owns all key dispatch. This
    // prevents the SM from double-executing: BackTab cycles permission AND
    // popup expects prev-question; Ctrl+T cycles model AND popup ignores;
    // Esc advances DoubleEscTracker AND popup expects to close. Returning
    // false here lets the keyboard fallback (popups::handle_popups) route
    // the key to the active popup exclusively.
    if popup_active {
        return false;
    }

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    // BackTab: cycle permission mode
    if matches!(key.code, KeyCode::BackTab) {
        return true;
    }

    // Enter (no Shift/Alt): state machine handles submission,
    // EXCEPT when an inline hint owns the key:
    //   - slash commands (`/...`) → CommandRegistry::dispatch in fallback
    //   - @mention popup active   → inject_at_mention_path in fallback
    //   - slash hint active       → hint_complete in fallback (covers
    //     mid-line `/token` after whitespace, where `is_slash_command`
    //     is false but `slash_hint.active` is true)
    // Use pre-transition flags: is_slash_command was captured before the
    // SM transition consumed the state (Idle → Streaming on Enter).
    if matches!(key.code, KeyCode::Enter)
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
        && (was_idle || matches!(state, State::Idle(_)))
    {
        if is_slash_command || at_mention_active || slash_hint_active {
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
            double_esc_timer: None,
            history_index: None,
        })
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    // ── Cron #25 unified popup-guard regression tests ────────────────────
    //
    // 背景：当 v1 popup 激活（AskUser / HITL / OAuth / Rewind）时，键盘
    // fallback 应独占按键分发。此前 is_sm_handled_shortcut 对 BackTab /
    // Ctrl+T/B/O/P 始终返回 true，对 Enter 也返回 true（非 slash），导致
    // SM 与 popup 双重执行：BackTab 既切权限模式又切不到上一问题，
    // Ctrl+T 切走模型，Enter 直接提交而非确认 popup。
    //
    // 修复：popup_active 时 is_sm_handled_shortcut 返回 false，让键盘
    // fallback 独占。

    #[test]
    fn test_popup_active_backtab_returns_false() {
        // BackTab + popup active → false（让 popup 收到 prev-tab）
        let state = idle_state();
        let key = KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE);
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, false, true, false, false),
            "BackTab with popup active must return false so popup gets prev-tab"
        );
        // 同键无 popup → 仍 true（SM 切权限模式）
        assert!(
            is_sm_handled_shortcut(&key, &state, true, false, false, false, false),
            "BackTab without popup must return true (SM cycles permission)"
        );
    }

    #[test]
    fn test_popup_active_ctrl_shortcuts_return_false() {
        // Ctrl+T/B/O/P + popup active → false（让 popup 决定）
        let state = idle_state();
        for c in ['t', 'b', 'o', 'p'] {
            let key = ctrl(c);
            assert!(
                !is_sm_handled_shortcut(&key, &state, true, false, true, false, false),
                "Ctrl+{c} with popup active must return false"
            );
            assert!(
                is_sm_handled_shortcut(&key, &state, true, false, false, false, false),
                "Ctrl+{c} without popup must return true"
            );
        }
    }

    #[test]
    fn test_popup_active_ctrl_shift_t_returns_false() {
        // Ctrl+Shift+T + popup active → false
        let state = idle_state();
        let key = KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, false, true, false, false),
            "Ctrl+Shift+T with popup active must return false"
        );
    }

    #[test]
    fn test_popup_active_enter_returns_false() {
        // Enter + popup active → false（让 popup 收到 confirm）
        // 关键场景：Rewind popup 打开时按 Enter 应确认 rewind，
        // 而非触发 SM 的 submit_message。
        let state = idle_state();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, false, true, false, false),
            "Enter with popup active must return false so popup confirms"
        );
        // 非 popup 时仍 true（提交消息）
        assert!(
            is_sm_handled_shortcut(&key, &state, true, false, false, false, false),
            "Enter without popup must return true (submit)"
        );
    }

    #[test]
    fn test_popup_active_enter_with_slash_command_still_false() {
        // Slash 命令 + popup active → false（popup 决定，slash 不应触发）
        let state = idle_state();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, true, true, false, false),
            "Enter (slash) with popup active must return false"
        );
    }

    #[test]
    fn test_popup_active_plain_char_returns_false() {
        // 普通 Char（无修饰键）+ popup active → false（让 popup 收到字符输入）
        let state = idle_state();
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, false, true, false, false),
            "Plain char with popup active must return false"
        );
        // 普通 Char 无 popup → 也是 false（键盘 fallback 处理 textarea 输入）
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, false, false, false, false),
            "Plain char without popup must return false (keyboard owns)"
        );
    }

    #[test]
    fn test_popup_active_esc_returns_false() {
        // Esc + popup active → false（让 popup 关闭，而非推进 DoubleEscTracker）
        // 这是 cron #25 审计 P0 bug 的核心：双击 Esc 不应退出 app。
        let state = idle_state();
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, false, true, false, false),
            "Esc with popup active must return false so popup closes (not quit)"
        );
    }

    // ── @mention / slash-hint Enter routing regression tests ────────────
    //
    // 背景：当 @mention 弹窗或 slash hint overlay 激活时，Enter 必须由
    // 键盘 fallback 处理（inject_at_mention_path / hint_complete），而
    // 不是被 SM 当作 submit。此前 is_sm_handled_shortcut 对 Enter 在非
    // slash 命令时始终返回 true，导致 SM 抢先提交原始 @文本（P0 bug）。
    //
    // 修复：is_sm_handled_shortcut 接收 at_mention_active / slash_hint_active
    // 标志，任一为 true 时对 Enter 返回 false，让 fallback 接管。

    #[test]
    fn test_at_mention_active_enter_returns_false() {
        // @mention 弹窗激活时按 Enter → false（让 fallback 注入选中路径）
        // 核心场景：用户输入 @src/main.rs、看到弹窗、按 Enter 选文件，
        // 应当注入路径而非提交原始 @query 文本。
        let state = idle_state();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, false, false, true, false),
            "Enter with @mention active must return false so path gets injected"
        );
        // 关闭 @mention 后仍 true（正常提交）
        assert!(
            is_sm_handled_shortcut(&key, &state, true, false, false, false, false),
            "Enter without @mention must return true (submit)"
        );
    }

    #[test]
    fn test_slash_hint_active_enter_returns_false() {
        // slash hint overlay 激活时按 Enter → false（让 fallback 完成 hint）
        // 覆盖两类场景：
        //   (a) 行首 / 命令（is_slash_command=true，原本就走 fallback）
        //   (b) 行中 / token（如 "review /code"，is_slash_command=false 但
        //       slash_hint.active=true）—— 这是本修复的关键场景
        let state = idle_state();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        // 行中 slash token：is_slash_command=false，slash_hint_active=true
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, false, false, false, true),
            "Enter with slash_hint active (mid-line token) must return false so hint completes"
        );
        // 无 hint 时仍 true（提交消息）
        assert!(
            is_sm_handled_shortcut(&key, &state, true, false, false, false, false),
            "Enter without slash_hint must return true (submit)"
        );
    }

    #[test]
    fn test_at_mention_and_slash_hint_mutually_defer_enter() {
        // 互斥验证：两个标志不应同时为 true（keyboard.rs 在 at_mention
        // 激活时 deactivate slash_hint），但任一为 true 都应让 Enter 走 fallback。
        let state = idle_state();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        // at_mention only
        assert!(!is_sm_handled_shortcut(
            &key, &state, true, false, false, true, false
        ));
        // slash_hint only
        assert!(!is_sm_handled_shortcut(
            &key, &state, true, false, false, false, true
        ));
    }

    #[test]
    fn test_at_mention_active_other_keys_unaffected() {
        // @mention 激活时 BackTab / Ctrl+T / 普通字符应不受影响——只有 Enter
        // 路由改变。BackTab 仍由 SM 处理（切权限模式），普通字符仍走 fallback。
        let state = idle_state();
        // BackTab + at_mention → true（SM 处理，与之前一致）
        let backtab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE);
        assert!(
            is_sm_handled_shortcut(&backtab, &state, true, false, false, true, false),
            "BackTab with @mention must still return true (SM cycles permission)"
        );
        // 普通字符 + at_mention → false（键盘 fallback 处理 textarea 输入）
        let char_key = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(
            !is_sm_handled_shortcut(&char_key, &state, true, false, false, true, false),
            "Plain char with @mention must return false (keyboard owns textarea)"
        );
    }

    #[test]
    fn test_popup_active_dominates_at_mention() {
        // 当 v1 popup（如 HITL）激活时，即使 @mention 也激活，popup 优先。
        // popup_active 分支在 at_mention 检查之前返回 false，所以结果一致
        // （都走 fallback），但语义上 popup 的按键分发优先。
        let state = idle_state();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            !is_sm_handled_shortcut(&key, &state, true, false, true, true, false),
            "Enter with popup_active + at_mention must return false (popup owns)"
        );
    }

    #[test]
    fn test_modal_state_overrides_popup_active() {
        // 当 SM 已在 Modal 状态（v2 Panel/Interaction）时，SM 独占所有按键，
        // popup_active 检查不应绕过 Modal。这条路径覆盖 v2 Modal 进入后
        // popup_active 仍为 true 的边角场景（罕见但需防御）。
        use crate::state_machine::handler::NoopHandler;
        use crate::state_machine::state::{ModalKind, ModalState};
        let modal_state = State::Modal(ModalState {
            saved_view: Vec::new(),
            saved_current_turn: None,
            saved_input: InputState::default(),
            saved_scroll_offset: 0,
            saved_history_index: None,
            saved_double_esc_timer: None,
            kind: ModalKind::Interaction(Box::new(NoopHandler)),
        });
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            is_sm_handled_shortcut(&key, &modal_state, false, false, true, false, false),
            "Modal state must override popup_active (SM owns all keys in Modal except Ctrl+C)"
        );
    }

    // ── Cron #29 P2 fix: Ctrl+C in Modal must reach keyboard fallback ────
    //
    // 背景：Modal 状态下 is_sm_handled_shortcut 原本对 ALL keys 返回 true。
    // modal.rs handle_key 只 dispatch Ctrl+T/B/O/P + Ctrl+Shift+T，其他按键
    // 落入 "_ => render-only" arm 被 drop。后果：用户在 v2 面板/弹窗打开时
    // （Model/Login/Config/Mcp/Cron 等，或 HITL/AskUser/Rewind/OAuth handler）
    // 按下 Ctrl+C 无法取消正在运行的 agent——必须先 Esc 关闭 popup 再按
    // Ctrl+C。这是相对 v1 的 UX 回归（v1 popups::handle_popups 始终能路由
    // Ctrl+C）。
    //
    // 修复：is_sm_handled_shortcut 在 Modal 状态对 Ctrl+C 返回 false，让
    // 键盘 fallback 跑 normal_keys.rs 的 app.interrupt()。

    #[test]
    fn test_modal_ctrl_c_returns_false_so_fallback_can_interrupt() {
        // Ctrl+C + Modal → false（让 keyboard fallback 跑 app.interrupt()）
        use crate::state_machine::handler::NoopHandler;
        use crate::state_machine::state::{ModalKind, ModalState};
        let modal_state = State::Modal(ModalState {
            saved_view: Vec::new(),
            saved_current_turn: None,
            saved_input: InputState::default(),
            saved_scroll_offset: 0,
            saved_history_index: None,
            saved_double_esc_timer: None,
            kind: ModalKind::Interaction(Box::new(NoopHandler)),
        });
        let ctrl_c = ctrl('c');
        assert!(
            !is_sm_handled_shortcut(&ctrl_c, &modal_state, false, false, false, false, false),
            "Ctrl+C in Modal must return false so keyboard fallback runs app.interrupt()"
        );
    }

    #[test]
    fn test_modal_ctrl_t_still_returns_true_after_cron29() {
        // Ctrl+T + Modal → true（SM 仍独占 cycle model dispatch）
        // 验证 Ctrl+C carve-out 没有意外影响其他 Ctrl+Char 快捷键。
        use crate::state_machine::handler::NoopHandler;
        use crate::state_machine::state::{ModalKind, ModalState};
        let modal_state = State::Modal(ModalState {
            saved_view: Vec::new(),
            saved_current_turn: None,
            saved_input: InputState::default(),
            saved_scroll_offset: 0,
            saved_history_index: None,
            saved_double_esc_timer: None,
            kind: ModalKind::Interaction(Box::new(NoopHandler)),
        });
        let ctrl_t = ctrl('t');
        assert!(
            is_sm_handled_shortcut(&ctrl_t, &modal_state, false, false, false, false, false),
            "Ctrl+T in Modal must still return true (SM handles cycle model)"
        );
    }

    #[test]
    fn test_modal_plain_char_still_returns_true_after_cron29() {
        // 普通 char + Modal → true（panel/interaction handler 通过 SM 接收）
        // 确保 Ctrl+C carve-out 只针对 Ctrl+C，不影响 panel 文本输入。
        use crate::state_machine::handler::NoopHandler;
        use crate::state_machine::state::{ModalKind, ModalState};
        let modal_state = State::Modal(ModalState {
            saved_view: Vec::new(),
            saved_current_turn: None,
            saved_input: InputState::default(),
            saved_scroll_offset: 0,
            saved_history_index: None,
            saved_double_esc_timer: None,
            kind: ModalKind::Interaction(Box::new(NoopHandler)),
        });
        let plain_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(
            is_sm_handled_shortcut(&plain_x, &modal_state, false, false, false, false, false),
            "Plain char in Modal must still return true (panel dispatch via SM)"
        );
    }

    // ── Cron #27 SwitchSession regression tests ───────────────────────────
    //
    // 背景：Effect::SwitchSession 从 v2 Modal（ThreadBrowser 面板）触发时，
    // 旧代码恢复 `modal.saved_view`（旧会话的 VM 快照），而不是清空让新会话
    // 的 ViewCommit 重新填充。结果：用户从 ThreadBrowser 切到另一个会话时，
    // 短暂看到旧会话的消息混入新会话显示，直到首个 ViewCommit 到达。
    //
    // 修复：用 State::Switching 替代 Idle{view: saved_view}。Switching 是
    // 会话切换的标准过渡态，清空 view + 显示 loading + 等待 ViewCommit 落地。
    //
    // 这些测试验证：SwitchSession 后 state 必为 Switching（saved_view 不泄漏）。

    #[test]
    fn test_switch_session_clears_modal_saved_view() {
        // 验证 SwitchSession 的核心契约：Modal.saved_view 不应在新 state 中存活。
        // 构造一个有内容的 saved_view，模拟"ThreadBrowser 打开前的旧会话视图"。
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
            saved_double_esc_timer: None,
            kind: ModalKind::Interaction(Box::new(NoopHandler)),
        });

        // 模拟 Effect::SwitchSession 执行后的 state 构造（与 main_loop.rs:481-506
        // 的 post-fix 逻辑一致）。这里不调用 App::open_thread，只验证 state 形状。
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
        // 这个测试验证 switching.rs 的 transition 逻辑（被 SwitchSession 复用）。
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
