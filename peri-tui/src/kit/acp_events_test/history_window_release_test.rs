//! S6.2：统一释放事务、holder 失效与渲染 ack。

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use super::*;
use crate::kit::history_window::{
    HistoryEntryView, HistorySource, HistoryWindow, HistoryWindowConfig, PersistentEntryKey,
    ReleasePlan, ReleaseReason, SegmentDraft, SegmentPlacement, WindowIdentity,
};
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};
use serial_test::serial;

fn bubble(text: &str) -> Arc<TuiAssistantBubble> {
    Arc::new(TuiAssistantBubble {
        text: text.to_string(),
        reasoning: None,
        message_id: Some("m1".into()),
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    })
}

fn release_fixture() -> (BridgeState, Weak<TuiAssistantBubble>, PersistentEntryKey) {
    crate::kit::atoms::init_atoms();
    crate::kit::acp_events::render::invalidate_structural_caches();
    crate::kit::atoms::STRUCTURAL_EPOCH.set(0);
    crate::kit::atoms::RENDER_ACHIEVED_EPOCH.set(0);
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    let key = PersistentEntryKey::own("s1:m1");
    let body = 128usize;
    let entry = HistoryEntryView::new(key.clone(), Some(10), 1, body, body, Vec::new());
    let segment_key = state
        .window
        .register_segment(
            SegmentDraft::new(HistorySource::Own, vec![entry], body, body),
            SegmentPlacement::Tail,
        )
        .expect("segment");
    state.window.mark_display_block(&[(key.clone(), 1)], 0);
    let segment = state.window.segment(segment_key).expect("segment").clone();
    let plan = ReleasePlan {
        segment: segment_key,
        source: segment.source.clone(),
        entries: segment.entry_keys(),
        unit_count: segment.unit_count,
        body_bytes: segment.body_bytes,
        reserved_bytes: segment.reserved_bytes,
        slot_range: state.window.index().slot_range(segment_key),
        before: None,
        after: None,
        before_cursor: None,
        after_cursor: None,
    };
    let weak = Arc::downgrade(&{
        let bubble = bubble("released body");
        state
            .committed
            .push_back(TuiRenderUnit::TuiAssistantBubble(Arc::clone(&bubble)));
        bubble
    });
    state
        .window
        .apply_release_plan(&plan)
        .expect("model release");
    (state, weak, key)
}

#[test]
#[serial]
fn release_removes_committed_and_invalidates_holders_before_ack() {
    let (mut state, weak, key) = release_fixture();
    // 先让渲染/折叠 holder 真正持有旧正文。
    crate::kit::acp_events::push_view_models(&mut state);
    let _folded =
        state
            .folded_history
            .project(&state.committed, SessionPhase::Idle, &HashMap::new());
    assert!(state.folded_history.debug_cached_len() > 0);
    assert!(state.window.displayed_range(&key).is_some());
    crate::kit::atoms::FOCUSED_ENTRY
        .set(Some(crate::kit::atoms::FocusedEntry { slot: 0, key: None }));
    crate::kit::acp_events::history_window::insert_pending_tool_end_for_tests(
        "released-tool",
        &"x".repeat(4096),
        false,
    );
    crate::kit::history_window::publish_window_unit_mappings(vec![
        crate::kit::history_window::WindowUnitMapping {
            token: crate::kit::history_window::WindowUnitToken { kind: 9, hash: 9 },
            entry: key.clone(),
        },
    ]);
    assert_ne!(
        crate::kit::acp_events::history_window::pending_tool_ends_fingerprint(),
        (0, 0)
    );

    let plans = state.window.take_pending_release_plans();
    let outcome = crate::kit::history_window::release_window_plans(
        &mut state,
        plans,
        ReleaseReason::BudgetPressure,
    );
    assert_eq!(outcome.released_units, 1);
    assert_eq!(outcome.structural_epoch, 1);
    assert!(state.committed.is_empty());
    assert!(state.window.displayed_range(&key).is_none());
    assert_eq!(state.folded_history.debug_cached_len(), 0);
    assert!(crate::kit::acp_events::render::debug_cache_lengths() == (None, None, None));
    assert!(crate::kit::atoms::FOCUSED_ENTRY.state().read().is_none());
    assert_eq!(state.window.pending_release_bytes(), 128);
    assert_eq!(crate::kit::atoms::STRUCTURAL_EPOCH.get(), 1);
    assert_eq!(
        crate::kit::acp_events::history_window::pending_tool_ends_fingerprint(),
        (0, 0)
    );
    assert!(
        crate::kit::atoms::WINDOW_UNIT_KEYS
            .state()
            .read()
            .iter()
            .all(|mapping| mapping.entry != key)
    );

    // 渲染侧发布新空快照并 ack 后才归还保守额度。
    crate::kit::atoms::RENDER_ACHIEVED_EPOCH.set(1);
    crate::kit::acp_events::push_view_models(&mut state);
    assert_eq!(state.window.pending_release_bytes(), 0);
    drop(_folded);
    assert!(
        weak.upgrade().is_none(),
        "last managed Arc must be gone after release + render ack"
    );
}

#[test]
#[serial]
fn release_does_not_touch_live_phase_or_input_queues() {
    let (mut state, _weak, _key) = release_fixture();
    state.phase = SessionPhase::PromptRunning;
    state.current_request_id = Some("req".into());
    state.turn_generation = 3;
    state.current_turn.append_text("live", Some("m"));
    let live_units = state.current_turn.view_models().clone();
    let plans = state.window.take_pending_release_plans();
    crate::kit::history_window::release_window_plans(
        &mut state,
        plans,
        ReleaseReason::BudgetPressure,
    );
    assert_eq!(state.phase, SessionPhase::PromptRunning);
    assert_eq!(state.current_request_id.as_deref(), Some("req"));
    assert_eq!(state.turn_generation, 3);
    assert_eq!(state.current_turn.view_models(), &live_units);
}

#[test]
#[serial]
fn release_ack_is_idempotent_and_late_ack_does_not_double_credit() {
    let (mut state, _weak, _key) = release_fixture();
    let plans = state.window.take_pending_release_plans();
    crate::kit::history_window::release_window_plans(
        &mut state,
        plans,
        ReleaseReason::BudgetPressure,
    );
    state.window.ack_release_epoch(1);
    assert_eq!(state.window.pending_release_bytes(), 0);
    state.window.ack_release_epoch(1);
    assert_eq!(state.window.pending_release_bytes(), 0);
}

#[test]
#[serial]
fn pending_release_keeps_budget_conservative_until_ack() {
    use crate::kit::history_window::{HistoryWindowConfig, WindowLimits};
    let mut window = HistoryWindow::new(HistoryWindowConfig {
        history_window_v1: true,
        limits: WindowLimits {
            body_bytes: 128,
            ..Default::default()
        },
    });
    window.record_pending_release(128, 1);
    assert_eq!(window.pending_release_bytes(), 128);
    assert!(
        !window.budget().has_body_capacity_for(1),
        "pending release must not credit body budget before ack"
    );
    window.ack_release_epoch(1);
    assert_eq!(window.pending_release_bytes(), 0);
    assert!(window.budget().has_body_capacity_for(1));
}
