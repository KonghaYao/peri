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

fn commit_info_at(position: u64) -> HistoryCommitInfo {
    let tail = HistoryCommittedTail::new(
        &ThreadId::from("s1".to_string()),
        MessageId::new(),
        position,
    )
    .encode();
    HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-1".into(),
        structure_version: 7,
        checkpoint_id: format!("cp-{position}"),
        summary_entry_id: None,
        committed_tail: tail,
    }
}

fn commit_info() -> HistoryCommitInfo {
    commit_info_at(10)
}

#[test]
#[serial]
fn test_commit_releases_only_outside_viewport_when_anchor_exists() {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    let mut keys = Vec::new();
    for (id, position) in [("a", 5), ("b", 10), ("c", 15)] {
        let key = PersistentEntryKey::own(format!("s1:{id}"));
        let segment = state
            .window
            .register_segment(
                SegmentDraft::new(
                    HistorySource::Own,
                    vec![HistoryEntryView::new(
                        key.clone(),
                        Some(position),
                        1,
                        10,
                        20,
                        Vec::new(),
                    )],
                    10,
                    20,
                ),
                SegmentPlacement::Tail,
            )
            .expect("segment");
        state
            .window
            .mark_display_block(&[(key.clone(), 1)], keys.len());
        state.committed.push_back(TuiRenderUnit::TuiUserBubble(
            crate::kit::tui_render_unit::TuiUserBubble::new(id.into()),
        ));
        keys.push((segment, key));
    }
    state
        .window
        .set_anchor(Some(crate::kit::history_window::WindowAnchor::new(
            keys[0].1.clone(),
            None,
            crate::kit::history_window::WindowAnchorOffset::CharOffset(0),
        )));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryCommitted(commit_info_at(15)),
    );
    assert_eq!(state.committed.len(), 2);
    assert!(state.window.segment(keys[2].0).is_none());
}

#[test]
#[serial]
fn test_commit_does_not_release_without_viewport_anchor() {
    let mut state = state_with_window();
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        crate::kit::tui_render_unit::TuiUserBubble::new("live".into()),
    ));
    state
        .window
        .mark_display_block(&[(PersistentEntryKey::own("s1:m1"), 1)], 0);
    dispatch_and_notify(&mut state, &AcpEventData::HistoryCommitted(commit_info()));
    assert_eq!(state.window.segment_count(), 1);
    assert_eq!(state.committed.len(), 1);
    assert_eq!(
        state
            .window
            .displayed_range(&PersistentEntryKey::own("s1:m1")),
        Some((0, 1))
    );
}

#[test]
#[serial]
fn test_tool_only_archive_counts_toward_budget_without_live_segment() {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-only".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    // 没有持久身份不能伪造可淘汰段，但正文必须计入总预算。
    assert_eq!(state.window.segment_count(), 0);
    assert!(
        state.window.budget().unattributed_live_bytes > 0,
        "tool-only archive must be counted in resident budget"
    );
    assert_eq!(
        state.window.diagnostics().tui_unattributed_live_bytes,
        state.window.budget().unattributed_live_bytes
    );
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

#[test]
#[serial]
fn live_archive_groups_split_message_and_releases_only_its_bubbles() {
    use crate::kit::history_window::{WindowAnchor, WindowAnchorOffset};
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    for id in ["anchor", "neighbor"] {
        state.current_turn.append_text(id, Some(id));
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    }
    state.current_turn.append_text("before", Some("split"));
    state
        .current_turn
        .start_tool(crate::kit::acp_types::ToolCardAccumulator::new(
            "tool-only".into(),
            "Read".into(),
            "file".into(),
        ));
    state.current_turn.append_text("after", Some("split"));
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = PersistentEntryKey::own("s1:split");
    let locator = *state.window.index().locate_entry(&key).unwrap();
    assert_eq!(locator.unit_count, 2);
    assert_eq!(
        state.window.segment_display_bounds(locator.segment),
        Some((2, 5))
    );
    let segment = state.window.segment(locator.segment).unwrap();
    assert_eq!(segment.body_bytes, "beforeafter".len());
    assert!(segment.reserved_bytes > segment.body_bytes);
    assert_eq!(state.committed.len(), 5);
    for (id, position) in [("anchor", 5), ("neighbor", 10), ("split", 15)] {
        assert!(state.window.promote_live_seal_position(
            &PersistentEntryKey::own(format!("s1:{id}")), position,
        ));
    }
    state.window.set_anchor(Some(WindowAnchor::new(
        PersistentEntryKey::own("s1:anchor"),
        None,
        WindowAnchorOffset::CharOffset(0),
    )));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryCommitted(commit_info_at(15)),
    );
    assert_eq!(state.committed.len(), 3);
    assert!(
        matches!(&state.committed[2], TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-only")
    );
    assert!(!state.window.index().contains_entry(&key));
    assert!(!state.window.is_entry_displayed(&key));
    assert!(
        crate::kit::atoms::WINDOW_UNIT_KEYS
            .state()
            .read()
            .iter()
            .all(|mapping| mapping.entry != key)
    );
}
