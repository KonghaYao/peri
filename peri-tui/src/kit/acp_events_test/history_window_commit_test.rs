//! S5.5：提交/compact/turn 事件不得误封印或触碰执行状态。

use super::*;
use crate::kit::history_window::{
    HistoryCommitInfo, HistoryEntryView, HistorySource, HistoryWindow, HistoryWindowConfig,
    PersistentEntryKey, SegmentDraft, SegmentPlacement, WindowIdentity,
};
use peri_acp_types::history_window::HistoryCommittedTail;
use peri_acp_types::messages::MessageId;
use peri_acp_types::thread::ThreadId;
use serial_test::serial;

fn state_with_window() -> BridgeState {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    state
        .window
        .register_segment(
            SegmentDraft::new(
                HistorySource::Own,
                vec![HistoryEntryView::new(
                    PersistentEntryKey::own("s1:m1"),
                    Some(10),
                    1,
                    10,
                    20,
                    Vec::new(),
                )],
                10,
                20,
            )
            .durable(false),
            SegmentPlacement::Tail,
        )
        .expect("live segment");
    state
}

fn commit_info() -> HistoryCommitInfo {
    let tail =
        HistoryCommittedTail::new(&ThreadId::from("s1".to_string()), MessageId::new(), 10).encode();
    HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-1".into(),
        structure_version: 7,
        checkpoint_id: "cp-1".into(),
        summary_entry_id: None,
        committed_tail: tail,
    }
}

#[test]
#[serial]
fn test_history_committed_seals_window_without_touching_execution_state() {
    let mut state = state_with_window();
    state.phase = SessionPhase::PromptRunning;
    state.current_request_id = Some("req-1".into());
    state.turn_generation = 5;
    state.last_prompt_generation = 3;
    state.last_successful_todo_sequence = Some(2);
    state.last_pushed_text_len = 17;
    let before_phase = state.phase;
    let before_request = state.current_request_id.clone();
    let before_generation = state.turn_generation;

    dispatch_and_notify(&mut state, &AcpEventData::HistoryCommitted(commit_info()));
    dispatch_and_notify(&mut state, &AcpEventData::HistoryCommitted(commit_info()));

    assert_eq!(state.window.sealed_segment_count(), 1);
    assert!(
        state
            .window
            .segment(*state.window.segments().keys().next().expect("segment"))
            .unwrap()
            .durable
    );
    assert_eq!(state.phase, before_phase);
    assert_eq!(state.current_request_id, before_request);
    assert_eq!(state.turn_generation, before_generation);
    assert_eq!(state.last_successful_todo_sequence, Some(2));
    assert_eq!(state.last_pushed_text_len, 17);
    assert_eq!(state.window.stats().admission_blocked, 0);
}

#[test]
#[serial]
fn test_compact_completed_does_not_seal_or_release_window_segments() {
    let mut state = state_with_window();
    let before_stats = state.window.stats();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CompactCompleted {
            summary: "summary".into(),
            messages_json: "[]".into(),
            trigger: "manual".into(),
            strategy: "full".into(),
            affected_count: 10,
            estimated_tokens_saved: 100,
            files: Vec::new(),
            skills: Vec::new(),
        },
    );
    assert_eq!(state.window.segment_count(), 1);
    assert_eq!(state.window.sealed_segment_count(), 0);
    assert!(
        !state
            .window
            .segment(*state.window.segments().keys().next().unwrap())
            .unwrap()
            .durable
    );
    assert_eq!(state.window.stats(), before_stats);
    // 该用例写入的是进程级完成提示；后续 serial 用例不得继承。
    crate::kit::atoms::PENDING_COMPACT_NOTE.set(None);
}

#[test]
#[serial]
fn test_turn_done_does_not_seal_live_window_segment() {
    let mut state = state_with_window();
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let segment = state
        .window
        .segment(*state.window.segments().keys().next().unwrap())
        .unwrap();
    assert!(!segment.durable);
    assert!(!segment.sealed);
    assert_eq!(state.window.sealed_segment_count(), 0);
}
