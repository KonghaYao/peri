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
    IdleState, ModalState, State,
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

    // Saved IdleState before entering Modal(Panel). Restored on panel close.
    let mut saved_idle: Option<IdleState> = None;

    while let Some(event) = rx.recv().await {
        let is_tick = matches!(event, TuiEvent::Tick);

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
        let fallback_effects = match &event {
            TuiEvent::Key(key)
                if !is_sm_handled_shortcut(key, &state, was_idle, is_slash_command) =>
            {
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
            TuiEvent::AcpEvent { event, data } => handle_acp_event(app, event, data),
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
                    // Transition Idle → Streaming so that incoming
                    // TextChunk / ReasoningChunk / ToolStarted events
                    // accumulate in current_turn instead of being dropped.
                    if let State::Idle(idle) = state {
                        state = State::Streaming(idle.into_streaming());
                    }
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
                                let mut new_provider =
                                    peri_acp::provider::config::ProviderConfig::default();
                                new_provider.id = id.to_string();
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
                    if matches!(state, State::Modal(_)) {
                        let idle = saved_idle.take().unwrap_or_else(|| {
                            let input_snapshot = crate::state_machine::input::sync::from_textarea(
                                &app.session_mgr.current().ui.textarea,
                            );
                            IdleState {
                                input: input_snapshot,
                                scroll_offset: app.session_mgr.current().ui.scroll_offset,
                                view: vec![],
                                double_esc_timer: None,
                                history_index: None,
                            }
                        });
                        state = State::Idle(idle);
                    }
                    needs_render = true;
                }
                Effect::OpenPanel(kind) => {
                    if let State::Idle(idle) = state {
                        saved_idle = Some(idle);
                        let panel = crate::panel::registry::create_panel(kind, app);
                        state = State::Modal(ModalState::Panel(panel));
                        needs_render = true;
                    }
                }
                Effect::ClosePanel => {
                    if matches!(state, State::Modal(_)) {
                        let idle = saved_idle.take().unwrap_or_else(|| {
                            // Fallback: when saved_idle is None (panel opened
                            // via fallback path), extract current input state
                            // from TextArea so keyboard buffer isn't lost.
                            let input_snapshot = crate::state_machine::input::sync::from_textarea(
                                &app.session_mgr.current().ui.textarea,
                            );
                            IdleState {
                                input: input_snapshot,
                                scroll_offset: app.session_mgr.current().ui.scroll_offset,
                                view: vec![],
                                double_esc_timer: None,
                                history_index: None,
                            }
                        });
                        state = State::Idle(idle);
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
                        use crate::app::MessageViewModel;
                        let session = app.session_mgr.current_mut();
                        session
                            .messages
                            .view_messages
                            .push(MessageViewModel::system(app.services.lc.tr_args(
                                "config-save-failed",
                                &[("error".into(), e.to_string().into())],
                            )));
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
                            use crate::app::MessageViewModel;
                            let session = app.session_mgr.current_mut();
                            session
                                .messages
                                .view_messages
                                .push(MessageViewModel::system(app.services.lc.tr_args(
                                    "config-save-failed",
                                    &[("error".into(), e.to_string().into())],
                                )));
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
                    // v2: Memory panel editor is managed by state machine
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
        // The keyboard module and effects (paste, mouse click) mutate the
        // TextArea widget directly. After all mutations are done, extract
        // the new text/cursor into the state machine.
        //
        // Only lines + cursor are synced from TextArea. Semantic fields
        // (prediction, at_mention, slash_completion, history, attachments)
        // are managed independently by the state machine and must NOT be
        // overwritten by the TextArea snapshot.
        {
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

    // Delegate to the App handler.
    let (updated, _should_break, should_return) = app.handle_acp_notification(notif);
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
