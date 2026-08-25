use super::*;
use crate::kit::acp_types::PendingInteraction;
use peri_acp_types::event_data::{AskUser, HitlPending};
use serial_test::serial;

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
