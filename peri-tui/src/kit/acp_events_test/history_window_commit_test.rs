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

/// [回归测试] 挂起时同步 subagent 仍在运行：稳定前缀归档、活动组保留在
/// current_turn，后续 stopped/done 仍能找到它并完成归档。
#[test]
#[serial]
fn test_turn_suspended_preserves_running_sync_subagent_and_registers_bytes() {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "stable-prefix".into(),
            message_id: None,
            agent_id: None,
        }),
    );
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
    assert!(state.current_turn.has_running_sync_subagent());

    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert!(
        state.current_turn.has_running_sync_subagent(),
        "挂起不得 reset 仍在运行的同步组"
    );
    assert!(
        state.committed.iter().any(|unit| matches!(
            unit,
            TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "stable-prefix"
        )),
        "子组之前的稳定正文必须归档"
    );
    assert!(
        state.window.budget().unattributed_live_bytes > 0,
        "归档正文必须计入驻留预算"
    );
    assert!(
        state
            .current_turn
            .view_models()
            .iter()
            .any(|unit| matches!(unit, TuiRenderUnit::TuiSubAgentGroup(_))),
        "活动组必须保留在 current_turn 等待后续事件"
    );

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child".into(),
            result: "done".into(),
            is_error: false,
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(
        state.committed.iter().any(|unit| matches!(
            unit,
            TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == "child"
        )),
        "stopped 后活动组必须在 turn 结束时完成归档"
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

/// 有 assistant 消息身份的工具卡归档后与正文同属一个可淘汰条目；提交封印后
/// 不再计入 `unattributed_live_bytes`，并随段一起释放。
#[test]
#[serial]
fn test_tool_card_archive_joins_assistant_message_segment_and_releases() {
    use crate::kit::history_window::{SegmentOrigin, WindowAnchor, WindowAnchorOffset};
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    for (text, message_id) in [("anchor", "m-anchor"), ("neighbor", "m-neighbor")] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: text.into(),
                message_id: Some(message_id.into()),
                agent_id: None,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    }
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "live answer".into(),
            message_id: Some("m-target".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-live".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    let key = PersistentEntryKey::own("s1:m-target");
    let locator = state
        .window
        .index()
        .locate_entry(&key)
        .copied()
        .expect("tool card must join the assistant entry");
    assert_eq!(locator.unit_count, 2, "text + tool card share one entry");
    let segment = state.window.segment(locator.segment).expect("segment");
    assert_eq!(segment.origin, SegmentOrigin::LiveSeal);
    assert!(
        segment.body_bytes > "live answer".len(),
        "tool card bytes must stay in the evictable body account"
    );
    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "attributed tool cards must not remain unattributed"
    );
    let body_before_release = state.window.budget().resident_body_bytes;

    for (id, position) in [("m-anchor", 5), ("m-neighbor", 10), ("m-target", 15)] {
        assert!(
            state
                .window
                .promote_live_seal_position(&PersistentEntryKey::own(format!("s1:{id}")), position)
        );
    }
    state.window.set_anchor(Some(WindowAnchor::new(
        PersistentEntryKey::own("s1:m-anchor"),
        None,
        WindowAnchorOffset::CharOffset(0),
    )));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryCommitted(commit_info_at(15)),
    );

    assert!(
        state.window.segment(locator.segment).is_none(),
        "sealed far segment released"
    );
    assert!(!state.window.index().contains_entry(&key));
    assert!(
        !state.committed.iter().any(
            |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-live")
        ),
        "released tool card leaves committed"
    );
    assert!(
        !state
            .committed
            .iter()
            .any(|unit| matches!(unit, TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "live answer"))
    );
    assert!(
        state.window.budget().resident_body_bytes < body_before_release,
        "releasing the segment returns its body bytes"
    );
    assert_eq!(state.window.budget().unattributed_live_bytes, 0);
}

/// 取消时整段归档的运行中工具卡收到迟到终点：已归档单元必须更新，窗口字节
/// 账目同步调整，不能留下 running 卡片或错误的驻留量。
#[test]
#[serial]
fn late_tool_end_updates_archived_card_and_segment_bytes() {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "cancelled body".into(),
            message_id: Some("m-cancel-tool".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-cancelled".into(),
            tool_name: "Bash".into(),
            input_summary: "run".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "cancel".into(),
            request_id: None,
        },
    );
    let key = PersistentEntryKey::own("s1:m-cancel-tool");
    let locator = *state.window.index().locate_entry(&key).expect("segment");
    let body_before = state.window.segment(locator.segment).unwrap().body_bytes;
    assert!(state.window.segment(locator.segment).unwrap().body_bytes > 0);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            tool_id: "tool-cancelled".into(),
            output_summary: "late tool output".into(),
            is_error: false,
            agent_id: None,
        }),
    );

    let card = state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-cancelled" => Some(card),
            _ => None,
        })
        .expect("archived card");
    assert_eq!(card.output_summary, "late tool output");
    assert!(!card.is_error);
    let segment = state.window.segment(locator.segment).expect("segment");
    assert!(
        segment.body_bytes > body_before,
        "completed card bytes must be reflected in the resident segment"
    );
    assert_eq!(
        state.window.budget().resident_body_bytes,
        segment.body_bytes
    );
}

/// 未归属工具卡字节被归属后必须从窗口账目回落；同一窗口代次重复记账只取增量。
#[test]
#[serial]
fn unattributed_tool_bytes_fall_back_after_claim() {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    let window_id = state.window.instance_id();
    let generation = state.window.generation();
    let bytes = 1_234usize;
    let delta = crate::kit::acp_events::live_tool_links::note_unattributed(
        "s1",
        window_id,
        generation,
        "tool-claim",
        bytes,
    );
    assert_eq!(delta, bytes as i64);
    state.window.note_unattributed_live_bytes(bytes);
    assert_eq!(state.window.budget().unattributed_live_bytes, bytes);
    assert_eq!(
        crate::kit::acp_events::live_tool_links::note_unattributed(
            "s1",
            window_id,
            generation,
            "tool-claim",
            bytes,
        ),
        0,
        "same window generation must only account the delta"
    );

    let taken = crate::kit::acp_events::live_tool_links::take_unattributed(
        "s1",
        window_id,
        generation,
        "tool-claim",
    );
    assert_eq!(taken, bytes);
    state.window.release_unattributed_live_bytes(taken);
    assert_eq!(state.window.budget().unattributed_live_bytes, 0);
    assert_eq!(
        crate::kit::acp_events::live_tool_links::take_unattributed(
            "s1",
            window_id,
            generation,
            "tool-claim",
        ),
        0,
        "claiming twice must not underflow the account"
    );
}

/// 没有 assistant 消息身份的工具卡（无文本/推理块的响应）仍不可淘汰：
/// 不得从 `tool_id` 猜测持久身份。
#[test]
#[serial]
fn test_tool_card_without_message_identity_stays_unattributed() {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    // 进程级 live 关联按会话保留：显式清掉消息身份，确保本用例从「无身份」开始。
    crate::kit::acp_events::live_tool_links::note_turn_boundary("s1");
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
            origin_message_id: None,
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

/// [回归测试] 同步子 Agent 正文仍没有持久补读通道，继续走未归属账目。
#[test]
#[serial]
fn test_sync_subagent_body_stays_unattributed() {
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "root answer".into(),
            message_id: Some("m-root".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child".into(),
            agent_name: "coder".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "child body".repeat(8),
            message_id: Some("m-child".into()),
            agent_id: Some("child".into()),
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child".into(),
            result: "done".into(),
            is_error: false,
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(
        state
            .window
            .index()
            .contains_entry(&PersistentEntryKey::own("s1:m-root")),
        "root text remains evictable"
    );
    assert!(
        state.window.budget().unattributed_live_bytes > 0,
        "sync subagent body has no persistent association and stays unattributed"
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

/// 跨 turn 归属不串：上一轮迟到的 `ToolEnded` 不得擦掉下一轮身份；两轮工具卡
/// 各自进入自己的 assistant 条目，`unattributed` 保持为 0。
#[test]
#[serial]
fn cross_turn_tool_ownership_does_not_bleed() {
    use crate::kit::history_window::SegmentOrigin;

    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));

    // 第 1 轮：文本 + 回退归属的工具卡（旧服务端无 originMessageId）。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "first".into(),
            message_id: Some("m-first".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-first".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            origin_message_id: None,
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // 第 2 轮：纯工具卡带真实来源；第 1 轮的 ToolEnded 迟到。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-second".into(),
            tool_name: "Grep".into(),
            input_summary: "pattern".into(),
            raw_input: serde_json::json!({}),
            origin_message_id: Some("m-second".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            tool_id: "tool-first".into(),
            output_summary: "late".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            tool_id: "tool-second".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    let first = PersistentEntryKey::own("s1:m-first");
    let second = PersistentEntryKey::own("s1:m-second");
    let first_locator = *state
        .window
        .index()
        .locate_entry(&first)
        .expect("第 1 轮工具归属第一个 assistant 消息");
    let second_locator = *state
        .window
        .index()
        .locate_entry(&second)
        .expect("第 2 轮纯工具卡归属自己的 assistant 消息");
    assert_ne!(first_locator.segment, second_locator.segment);
    assert_eq!(
        state.window.segment(first_locator.segment).unwrap().origin,
        SegmentOrigin::LiveSeal
    );
    assert_eq!(
        state.window.segment(second_locator.segment).unwrap().origin,
        SegmentOrigin::LiveSeal
    );
    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "两轮工具卡都已归属，不得残留未归属账目"
    );
}
