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

/// 当前 turn 已有带持久 message_id 的正文、窗口已启用的状态。
fn state_with_live_message_text(message_id: &str, text: &str) -> BridgeState {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    state.current_turn.append_text(text, Some(message_id));
    state
}

/// 带 message_id 的正文归档后，窗口 resident body 必须与本次新增正文精确一致。
#[test]
#[serial]
fn test_turn_suspended_registers_exact_body_bytes_for_message() {
    let text = "exact-suspend-body-".repeat(16);
    let mut state = state_with_live_message_text("m-suspend", &text);
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert_eq!(state.window.budget().resident_body_bytes, text.len());
}

#[test]
#[serial]
fn test_turn_interrupted_registers_exact_body_bytes_for_message() {
    let text = "exact-cancel-body-".repeat(16);
    let mut state = state_with_live_message_text("m-cancel", &text);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "cancel".into(),
            request_id: None,
        },
    );
    assert_eq!(state.window.budget().resident_body_bytes, text.len());
}

#[test]
#[serial]
fn test_stale_turn_interrupted_registers_exact_body_bytes_for_message() {
    let text = "exact-stale-body-".repeat(16);
    let mut state = state_with_live_message_text("m-stale", &text);
    state.turn_generation = 2;
    state.last_prompt_generation = 1;
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "cancel".into(),
            request_id: None,
        },
    );
    assert_eq!(state.window.budget().resident_body_bytes, text.len());
}

/// 重复 suspend / flush 不得重复登记同一正文（segment 与字节都不翻倍）。
#[test]
#[serial]
fn test_repeated_suspend_and_flush_do_not_double_register() {
    let text = "repeat-body-".repeat(16);
    let mut state = state_with_live_message_text("m-repeat", &text);
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    let segments_once = state.window.segment_count();
    let bytes_once = state.window.budget().resident_body_bytes;
    assert_eq!(bytes_once, text.len());

    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(state.window.segment_count(), segments_once);
    assert_eq!(state.window.budget().resident_body_bytes, bytes_once);
    assert_eq!(state.window.sealed_segment_count(), 0);
}

/// 当前 turn 已有未归档正文、窗口已启用的状态（suspend / stale 归档用例共用）。
fn state_with_live_text() -> BridgeState {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    state
        .current_turn
        .append_text(&"suspend-body-".repeat(32), None);
    state
}

/// [回归测试] 挂起归档必须与正常结束共用窗口登记：正文驻留不能被漏记。
#[test]
#[serial]
fn test_turn_suspended_archive_registers_resident_bytes() {
    let mut state = state_with_live_text();
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert!(!state.committed.is_empty(), "挂起前产出的正文必须归档");
    assert!(
        state.window.budget().unattributed_live_bytes > 0,
        "没有持久身份的子组/正文也必须计入驻留预算"
    );
    assert_eq!(
        state.window.diagnostics().tui_unattributed_live_bytes,
        state.window.budget().unattributed_live_bytes
    );
}

/// [回归测试] 非 stale 中断的归档路径同样必须登记窗口预算。
#[test]
#[serial]
fn test_turn_interrupted_archive_registers_resident_bytes() {
    let mut state = state_with_live_text();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "cancel".into(),
            request_id: None,
        },
    );
    assert!(!state.committed.is_empty(), "中断前产出的正文必须归档");
    assert!(
        state.window.budget().unattributed_live_bytes > 0,
        "非 stale 中断归档同样不得绕过驻留预算登记"
    );
}

/// [回归测试] 带运行中 subagent 的挂起仍保持冻结语义：子组归档、trailing
/// 冻结为 Completed 形态，正文计入窗口预算；不因统一入口而无限保留。
#[test]
#[serial]
fn test_turn_suspended_freezes_running_subagent_and_registers_bytes() {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child".into(),
            agent_name: "coder".into(),
            is_background: false,
            parent_tool_call_id: Some("parent-call-1".into()),
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "child-streaming-body".repeat(8),
            message_id: Some("child-message".into()),
            agent_id: Some("child".into()),
        }),
    );
    assert!(state.current_turn.has_running_subagent());

    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert!(
        state.current_turn.is_empty(),
        "挂起仍要 reset current_turn（保留既有冻结语义）"
    );
    let group = state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiSubAgentGroup(group) => Some(group),
            _ => None,
        })
        .expect("运行中子组必须作为冻结快照归档");
    assert!(
        group.is_running,
        "子组仍标记 running（subagent-stopped 未到）"
    );
    let bubble = group
        .view_models
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble),
            _ => None,
        })
        .expect("子组正文必须保留");
    assert!(!bubble.text.is_empty(), "挂起归档必须保留子组已产出的正文");
    assert!(
        state.window.budget().unattributed_live_bytes > 0,
        "带运行中子组的挂起正文必须计入驻留预算"
    );
}

/// [回归测试] 迟到（stale）中断的归档路径同样必须登记窗口预算。
#[test]
#[serial]
fn test_stale_turn_interrupted_archive_registers_resident_bytes() {
    let mut state = state_with_live_text();
    state.turn_generation = 2;
    state.last_prompt_generation = 1;
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "cancel".into(),
            request_id: None,
        },
    );
    assert!(!state.committed.is_empty(), "stale 前产出的正文必须归档");
    assert!(
        state.window.budget().unattributed_live_bytes > 0,
        "stale 归档不得绕过驻留预算登记"
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
        assert!(state
            .window
            .promote_live_seal_position(&PersistentEntryKey::own(format!("s1:{id}")), position,));
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
