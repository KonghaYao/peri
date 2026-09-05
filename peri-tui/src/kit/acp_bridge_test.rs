use super::*;
use crate::kit::acp_types::PendingInteraction;
use peri_acp_types::event_data::{AskUser, HitlPending};
use serial_test::serial;

fn scheduler_state() -> BridgeState {
    crate::kit::atoms::init_atoms();
    BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::PromptRunning,
        popup_kind: None,
        generation: 0,
        active_session_id: "s1".into(),
        compact_just_completed: false,
        last_submitted_text: None,
        last_pushed_text_len: 0,
        last_pushed_reasoning_len: 0,
        last_successful_todos: None,
        last_successful_todo_sequence: None,
        next_todo_sequence: 0,
        todo_call_inputs: Default::default(),
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
        pending_cache_usage: None,
    }
}

#[test]
#[serial]
fn test_production_scheduler_uses_fixed_deadline_and_terminal_invalidates_it() {
    use crate::kit::atoms::VIEW_MODELS;

    *VIEW_MODELS.state().write() = Default::default();
    let mut state = scheduler_state();
    let mut scheduler = PublicationScheduler::default();

    state.current_turn.append_text("first", Some("m1"));
    scheduler.accept(PublicationIntent::Immediate, &mut state);
    assert_eq!(state.generation, 1);

    let now = tokio::time::Instant::now();
    state.current_turn.append_text(" second", Some("m1"));
    scheduler.accept_at(PublicationIntent::Deferred, &mut state, now);
    let fixed_deadline = scheduler.pending_deadline.expect("deadline scheduled");
    state.current_turn.append_text(" third", Some("m1"));
    scheduler.accept_at(
        PublicationIntent::Deferred,
        &mut state,
        now + std::time::Duration::from_millis(25),
    );
    assert_eq!(scheduler.pending_deadline, Some(fixed_deadline));

    assert!(!scheduler.fire_at(&mut state, now + std::time::Duration::from_millis(49)));
    assert_eq!(state.generation, 1);
    assert!(scheduler.fire_at(&mut state, fixed_deadline));
    assert_eq!(state.generation, 2);
    assert_eq!(state.current_turn.text, "first second third");

    state.current_turn.append_text(" final", Some("m1"));
    scheduler.accept(PublicationIntent::Deferred, &mut state);
    scheduler.accept(PublicationIntent::Immediate, &mut state);
    assert!(scheduler.pending_deadline.is_none());
    let terminal_generation = state.generation;
    assert_eq!(state.generation, terminal_generation);
}

#[test]
#[serial]
fn test_production_scheduler_lifecycle_invalidation_matrix_is_stale_noop() {
    use crate::kit::atoms::VIEW_MODELS;

    for lifecycle in ["terminal", "reset", "session", "shutdown"] {
        *VIEW_MODELS.state().write() = Default::default();
        let mut state = scheduler_state();
        state.current_turn.append_text("pending", Some("m1"));
        let mut scheduler = PublicationScheduler::default();
        let now = tokio::time::Instant::now();
        scheduler.accept_at(PublicationIntent::Deferred, &mut state, now);
        let stale_deadline = scheduler.pending_deadline.unwrap();

        match lifecycle {
            "terminal" => scheduler.accept_at(PublicationIntent::Immediate, &mut state, now),
            "reset" | "session" | "shutdown" => scheduler.invalidate(),
            _ => unreachable!(),
        }
        let generation = state.generation;
        assert!(!scheduler.fire_at(&mut state, stale_deadline));
        assert_eq!(state.generation, generation, "lifecycle={lifecycle}");
    }
}

#[test]
#[serial]
fn test_receiver_close_reset_wins_over_dirty_final_publication() {
    use crate::kit::atoms::{ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, VIEW_MODELS};

    let old_reset = BRIDGE_RESET_COUNTER.get();
    *ACTIVE_SESSION_ID.state().write() = "s2".into();
    *VIEW_MODELS.state().write() = Default::default();
    let mut state = scheduler_state();
    state.current_turn.append_text("stale", Some("m1"));
    let mut last_reset = old_reset;
    let new_reset = old_reset.wrapping_add(1);
    BRIDGE_RESET_COUNTER.set(new_reset);

    let mut scheduler = PublicationScheduler::default();
    scheduler.accept_at(
        PublicationIntent::Deferred,
        &mut state,
        tokio::time::Instant::now(),
    );
    flush_on_receiver_close(&mut state, &mut scheduler, &mut last_reset);

    assert!(scheduler.pending_deadline.is_none());
    assert_eq!(state.active_session_id, "s2");
    assert!(VIEW_MODELS.state().read().items.is_empty());
    BRIDGE_RESET_COUNTER.set(old_reset);
}

#[test]
fn test_deterministic_clock_advances_without_sleep() {
    let mut clock = DeterministicClock::default();
    clock.advance_ms(50);
    assert_eq!(clock.now_ms(), 50);
}

#[test]
fn test_publication_observer_records_metadata_without_content() {
    reset_perf_counters();
    let observations = [
        PublicationObservation {
            generation: 7,
            source_version: 11,
            reason: PublicationReason::Intermediate,
        },
        PublicationObservation {
            generation: 8,
            source_version: 12,
            reason: PublicationReason::Terminal,
        },
        PublicationObservation {
            generation: 0,
            source_version: 13,
            reason: PublicationReason::Reset,
        },
    ];
    for observation in observations {
        observe_publication(observation);
    }
    let counters = perf_counters();
    assert_eq!(
        (
            observations,
            counters.intermediate_publications,
            counters.terminal_publications,
            counters.reset_publications,
        ),
        (observations, 1, 1, 1)
    );
}

fn hitl() -> AcpEventData {
    AcpEventData::HitlPending(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"h\"".into(),
        payload: HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::Value::Null,
            batch: None,
        },
    })
}

fn ask_user() -> AcpEventData {
    AcpEventData::AskUser(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"a\"".into(),
        payload: AskUser { questions: vec![] },
    })
}

/// [回归测试] 入站 interaction 只接受非空且精确匹配的 active session。
#[test]
fn test_interaction_gate_requires_nonempty_exact_active_session() {
    for event in [hitl(), ask_user()] {
        assert!(accepts_event_session(&event, "s1", "s1", false));
        assert!(!accepts_event_session(&event, "", "s1", false));
        assert!(!accepts_event_session(&event, "s1", "", false));
        assert!(!accepts_event_session(&event, "s1", "s2", false));
        assert!(accepts_event_session(&event, "s1", "s1", true));
    }
}

#[test]
fn test_ordinary_gate_preserves_nonreset_wildcards() {
    let event = AcpEventData::InteractionTerminal {
        owner: Default::default(),
        outcome: crate::acp_client::InteractionUiOutcome::Resolved {
            result: "done".into(),
        },
    };
    assert!(accepts_event_session(&event, "", "s1", false));
    assert!(accepts_event_session(&event, "s1", "", false));
    assert!(accepts_event_session(&event, "s1", "s1", false));
    assert!(!accepts_event_session(&event, "s1", "s2", false));
}

#[test]
fn test_ordinary_gate_preserves_just_reset_rules() {
    let event = AcpEventData::PromptStarted;
    assert!(accepts_event_session(&event, "s1", "", true));
    assert!(accepts_event_session(&event, "s1", "s1", true));
    assert!(!accepts_event_session(&event, "", "s1", true));
    assert!(!accepts_event_session(&event, "s1", "s2", true));
}

fn goal_snapshot(objective: &str, continuation_count: u64) -> AcpEventData {
    AcpEventData::GoalSnapshot {
        objective: Some(objective.into()),
        status: Some(peri_acp_types::goal::GoalStatus::Active),
        token_budget: None,
        tokens_used: 0,
        time_used_seconds: 0,
        continuation_count,
        blocked_reason: None,
    }
}

/// Goal 投影必须服从普通事件的 session ownership gate。
#[tokio::test]
#[serial]
async fn test_goal_snapshot_bridge_accepts_current_session_and_drops_stale_session() {
    use crate::kit::atoms::{ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, GOAL_SNAPSHOT};

    let old_active = ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_goal = GOAL_SNAPSHOT.state().read().clone();
    *ACTIVE_SESSION_ID.state().write() = "s1".into();
    *GOAL_SNAPSHOT.state().write() = None;
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(1));

    let (tx, rx) = mpsc::unbounded_channel();
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_acp_bridge_observed(rx, shutdown.clone(), observed_tx);

    tx.send(AcpEventWithEpoch {
        event: goal_snapshot("current", 2),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    assert_eq!(
        GOAL_SNAPSHOT
            .state()
            .read()
            .as_ref()
            .and_then(|goal| goal.objective.as_deref()),
        Some("current")
    );

    tx.send(AcpEventWithEpoch {
        event: goal_snapshot("stale", 99),
        active_session_id: "s0".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    let projected = GOAL_SNAPSHOT.state().read().clone().unwrap();
    assert_eq!(projected.objective.as_deref(), Some("current"));
    assert_eq!(projected.continuation_count, 2);

    shutdown.cancel();
    drop(tx);
    handle.await.unwrap();
    *ACTIVE_SESSION_ID.state().write() = old_active;
    BRIDGE_RESET_COUNTER.set(old_reset);
    *GOAL_SNAPSHOT.state().write() = old_goal;
}

/// [回归测试] production bridge 在 session gate 前不发布 HITL UI state。
#[tokio::test]
#[serial]
async fn test_hitl_bridge_drops_unowned_events_before_all_side_effects() {
    use crate::kit::atoms::{
        ACP_STATE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, FOCUSED_ENTRY, FOLD_OVERRIDES,
        HITL_PENDING, INPUT_BUFFER, PENDING_COMPACT_NOTE, POPUP_KIND, PopupKind, VIEW_MODELS,
    };
    let old_acp_state = ACP_STATE.state().read().clone();
    let old_active = ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_input = INPUT_BUFFER.state().read().clone();
    let old_fold_overrides = FOLD_OVERRIDES.state().read().clone();
    let old_focused_entry = FOCUSED_ENTRY.state().read().clone();
    let old_compact_note = PENDING_COMPACT_NOTE.state().read().clone();
    let old_pending = HITL_PENDING.state().read().clone();
    let old_popup = *POPUP_KIND.state().read();
    let old_view = VIEW_MODELS.state().read().clone();
    *ACTIVE_SESSION_ID.state().write() = String::new();
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(1));
    let (tx, rx) = mpsc::unbounded_channel();
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_acp_bridge_observed(rx, shutdown.clone(), observed_tx);
    tx.send(AcpEventWithEpoch {
        event: AcpEventData::PromptStarted,
        active_session_id: String::new(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    *HITL_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel\"".into(),
        payload: HitlPending {
            tool_name: "sentinel".into(),
            tool_input: serde_json::Value::Null,
            batch: None,
        },
    });
    *POPUP_KIND.state().write() = Some(PopupKind::OAuth);
    let sentinel_view = VIEW_MODELS.state().read().clone();
    tx.send(AcpEventWithEpoch {
        event: hitl(),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    assert_eq!(
        HITL_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel\""
    );
    assert_eq!(*POPUP_KIND.state().read(), Some(PopupKind::OAuth));
    assert_eq!(
        VIEW_MODELS.state().read().items.len(),
        sentinel_view.items.len()
    );
    *ACTIVE_SESSION_ID.state().write() = "s1".into();
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(2));
    tx.send(AcpEventWithEpoch {
        event: AcpEventData::PromptStarted,
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    *HITL_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel\"".into(),
        payload: HitlPending {
            tool_name: "sentinel".into(),
            tool_input: serde_json::Value::Null,
            batch: None,
        },
    });
    *POPUP_KIND.state().write() = Some(PopupKind::OAuth);
    tx.send(AcpEventWithEpoch {
        event: hitl(),
        active_session_id: "stale".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    assert_eq!(
        HITL_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel\""
    );
    assert_eq!(*POPUP_KIND.state().read(), Some(PopupKind::OAuth));
    tx.send(AcpEventWithEpoch {
        event: hitl(),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    assert_eq!(
        HITL_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"h\""
    );
    assert_eq!(*POPUP_KIND.state().read(), Some(PopupKind::Hitl));
    assert!(VIEW_MODELS.state().read().items.iter().any(|vm| matches!(vm, crate::kit::tui_render_unit::TuiRenderUnit::TuiAskUserBlock(block) if block.request_id.as_deref() == Some("\"h\""))));
    shutdown.cancel();
    drop(tx);
    handle.await.unwrap();
    *ACP_STATE.state().write() = old_acp_state;
    *ACTIVE_SESSION_ID.state().write() = old_active;
    BRIDGE_RESET_COUNTER.set(old_reset);
    *INPUT_BUFFER.state().write() = old_input;
    *FOLD_OVERRIDES.state().write() = old_fold_overrides;
    *FOCUSED_ENTRY.state().write() = old_focused_entry;
    *PENDING_COMPACT_NOTE.state().write() = old_compact_note;
    *HITL_PENDING.state().write() = old_pending;
    *POPUP_KIND.state().write() = old_popup;
    *VIEW_MODELS.state().write() = old_view;
}

/// [回归测试] production bridge 在 session gate 前不发布 AskUser UI state。
#[tokio::test]
#[serial]
async fn test_ask_user_bridge_drops_unowned_events_before_all_side_effects() {
    use crate::app::panel_types::PanelKind;
    use crate::kit::atoms::{
        ACP_STATE, ACTIVE_PANEL, ACTIVE_SESSION_ID, ASK_USER_PENDING, BRIDGE_RESET_COUNTER,
        FOCUSED_ENTRY, FOLD_OVERRIDES, INPUT_BUFFER, OPEN_PANELS, PENDING_COMPACT_NOTE,
        VIEW_MODELS,
    };
    let old_acp_state = ACP_STATE.state().read().clone();
    let old_active_session = ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_input = INPUT_BUFFER.state().read().clone();
    let old_fold_overrides = FOLD_OVERRIDES.state().read().clone();
    let old_focused_entry = FOCUSED_ENTRY.state().read().clone();
    let old_compact_note = PENDING_COMPACT_NOTE.state().read().clone();
    let old_pending = ASK_USER_PENDING.state().read().clone();
    let old_active_panel = *ACTIVE_PANEL.state().read();
    let old_open = OPEN_PANELS.state().read().clone();
    let old_view = VIEW_MODELS.state().read().clone();
    *ACTIVE_SESSION_ID.state().write() = String::new();
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(1));
    let (tx, rx) = mpsc::unbounded_channel();
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_acp_bridge_observed(rx, shutdown.clone(), observed_tx);
    tx.send(AcpEventWithEpoch {
        event: AcpEventData::PromptStarted,
        active_session_id: String::new(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    *ASK_USER_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel\"".into(),
        payload: AskUser { questions: vec![] },
    });
    *OPEN_PANELS.state().write() = vec![PanelKind::Tasks];
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Tasks);
    let sentinel_view = VIEW_MODELS.state().read().clone();
    tx.send(AcpEventWithEpoch {
        event: ask_user(),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    assert_eq!(
        ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel\""
    );
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Tasks));
    assert_eq!(
        VIEW_MODELS.state().read().items.len(),
        sentinel_view.items.len()
    );
    *ACTIVE_SESSION_ID.state().write() = "s1".into();
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(2));
    tx.send(AcpEventWithEpoch {
        event: AcpEventData::PromptStarted,
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    *ASK_USER_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel\"".into(),
        payload: AskUser { questions: vec![] },
    });
    *OPEN_PANELS.state().write() = vec![PanelKind::Tasks];
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Tasks);
    tx.send(AcpEventWithEpoch {
        event: ask_user(),
        active_session_id: "stale".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    assert_eq!(
        ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel\""
    );
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Tasks));
    tx.send(AcpEventWithEpoch {
        event: ask_user(),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    assert_eq!(
        ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"a\""
    );
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::AskUser));
    shutdown.cancel();
    drop(tx);
    handle.await.unwrap();
    *ACP_STATE.state().write() = old_acp_state;
    *ACTIVE_SESSION_ID.state().write() = old_active_session;
    BRIDGE_RESET_COUNTER.set(old_reset);
    *INPUT_BUFFER.state().write() = old_input;
    *FOLD_OVERRIDES.state().write() = old_fold_overrides;
    *FOCUSED_ENTRY.state().write() = old_focused_entry;
    *PENDING_COMPACT_NOTE.state().write() = old_compact_note;
    *ASK_USER_PENDING.state().write() = old_pending;
    *ACTIVE_PANEL.state().write() = old_active_panel;
    *OPEN_PANELS.state().write() = old_open;
    *VIEW_MODELS.state().write() = old_view;
}
