use super::*;
use crate::kit::display_retention::{MAX_BYTES, MAX_ENTRIES};
use crate::kit::history_window::{
    HistoryEntryView, HistorySource, HistoryWindow, HistoryWindowConfig, PersistentEntryKey,
    ReleasePlan, ReleaseReason, SegmentDraft, SegmentPlacement, WindowIdentity,
};
use crate::kit::retained_bytes::{SeenAllocations, retained_units_bytes};

fn state() -> BridgeState {
    let mut state = make_fold_test_state();
    state.active_session_id = "retention".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("retention", "g", 1, "s"));
    state
}

fn note(state: &mut BridgeState, text: String) {
    dispatch_and_notify(
        state,
        &AcpEventData::CommandFeedback(TuiCommandFeedback {
            message: text,
            level: FeedbackLevel::Info,
            channel: FeedbackChannel::UiOnly,
        }),
    );
}

fn retained(state: &BridgeState) -> usize {
    retained_units_bytes(state.committed.iter(), &mut SeenAllocations::default())
}

#[test]
#[serial]
fn test_display_retention_notes_plateau_across_turns_without_spending_canonical_budget() {
    let mut state = state();
    for index in 0..2048 {
        note(&mut state, format!("notice-{index}"));
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    }
    assert_eq!(state.committed.len(), MAX_ENTRIES);
    assert!(retained(&state) <= MAX_BYTES);
    assert_eq!(state.window.budget().unattributed_live_bytes, 0);
    let snapshot = VIEW_MODELS.state().read().clone();
    assert!(snapshot.items.iter().any(|unit| {
        matches!(unit, TuiRenderUnit::TuiSystemNote(note) if note.text == "notice-2047")
    }));
    assert!(!snapshot.items.iter().any(|unit| {
        matches!(unit, TuiRenderUnit::TuiSystemNote(note) if note.text == "notice-0")
    }));
}

#[test]
#[serial]
fn test_display_retention_pending_notes_do_not_split_streaming_text() {
    let mut state = state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "before ".into(),
            message_id: Some("answer".into()),
            agent_id: None,
        }),
    );
    for index in 0..1024 {
        note(&mut state, format!("note-{index}: {}", "中".repeat(2048)));
    }
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "after".into(),
            message_id: Some("answer".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let texts: Vec<_> = state
        .committed
        .iter()
        .filter_map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["before after"]);
    let notes: Vec<_> = state
        .committed
        .iter()
        .filter(|unit| matches!(unit, TuiRenderUnit::TuiSystemNote(_)))
        .collect();
    assert!(notes.len() < MAX_ENTRIES, "字节预算先于条数预算触发");
    assert!(retained_units_bytes(notes.into_iter(), &mut SeenAllocations::default()) <= MAX_BYTES);
}

#[test]
#[serial]
fn test_display_retention_completed_approvals_expire_but_pending_owner_still_resolves() {
    let mut state = state();
    let pending_owner = crate::acp_client::InteractionOwner {
        token: 9000,
        ..Default::default()
    };
    let pending = |owner: crate::acp_client::InteractionOwner| {
        AcpEventData::HitlPending(crate::kit::acp_types::PendingInteraction {
            request_id_json: format!("request-{}", owner.token),
            owner,
            payload: HitlPending {
                tool_name: "Bash".into(),
                tool_input: json!({"command": "echo ok"}),
                batch: None,
            },
        })
    };
    dispatch_and_notify(&mut state, &pending(pending_owner.clone()));
    for token in 0..1024 {
        let owner = crate::acp_client::InteractionOwner {
            token,
            ..Default::default()
        };
        dispatch_and_notify(&mut state, &pending(owner.clone()));
        dispatch_and_notify(
            &mut state,
            &AcpEventData::InteractionTerminal {
                owner,
                outcome: crate::acp_client::InteractionUiOutcome::Resolved {
                    result: "Allowed".into(),
                },
            },
        );
    }
    assert_eq!(state.committed.len(), MAX_ENTRIES + 1);
    assert!(state.committed.iter().any(|unit| {
        matches!(unit, TuiRenderUnit::TuiAskUserBlock(block)
            if block.pending && block.owner.as_ref() == Some(&pending_owner))
    }));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::InteractionTerminal {
            owner: pending_owner,
            outcome: crate::acp_client::InteractionUiOutcome::Resolved {
                result: "Allowed late".into(),
            },
        },
    );
    assert_eq!(state.committed.len(), MAX_ENTRIES);
    assert!(
        !state
            .committed
            .iter()
            .any(|unit| { matches!(unit, TuiRenderUnit::TuiAskUserBlock(block) if block.pending) })
    );
}

#[test]
#[serial]
fn test_display_retention_shifts_canonical_ranges_and_preserves_releasable_history() {
    let mut state = state();
    note(&mut state, "local-old".into());
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    note(&mut state, "durable-note".into());
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = PersistentEntryKey::own("retention:durable");
    let entry = HistoryEntryView::new(key.clone(), Some(1), 1, 32, 64, Vec::new());
    let segment_key = state
        .window
        .register_segment(
            SegmentDraft::new(HistorySource::Own, vec![entry], 32, 64),
            SegmentPlacement::Tail,
        )
        .unwrap();
    assert!(state.window.mark_existing_display_slot(key.clone(), 1, 1));
    for index in 0..MAX_ENTRIES {
        note(&mut state, format!("new-{index}"));
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    }
    assert_eq!(state.window.displayed_range(&key), Some((0, 1)));
    assert!(
        matches!(&state.committed[0], TuiRenderUnit::TuiSystemNote(note) if note.text == "durable-note")
    );
    let segment = state.window.segment(segment_key).unwrap().clone();
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
    let outcome = crate::kit::history_window::release_window_plans(
        &mut state,
        vec![plan],
        ReleaseReason::BudgetPressure,
    );
    assert_eq!(outcome.released_units, 1);
    assert_eq!(state.committed.len(), MAX_ENTRIES);
    assert!(state.committed.iter().all(|unit| {
        matches!(unit, TuiRenderUnit::TuiSystemNote(note) if note.text.starts_with("new-"))
    }));
}

#[test]
#[serial]
fn test_display_retention_disabled_window_keeps_legacy_paging_spans() {
    crate::kit::history_paging::reset_browsing_state();
    let mut state = make_fold_test_state();
    state.active_session_id = "retention-legacy".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::default());
    assert!(!state.window.is_enabled());
    // 关闭窗口时 legacy 分页仍以 HISTORY_SPANS 判重、定插入点。
    crate::kit::history_paging::mark_own_entry("legacy:own", 1000);
    crate::kit::history_paging::note_own_entry_unit(400);
    crate::kit::history_paging::close_own_entry();
    for index in 0..MAX_ENTRIES + 64 {
        note(&mut state, format!("legacy-note-{index}"));
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    }
    assert!(
        crate::kit::history_paging::with_spans(|spans| spans.contains("legacy:own")),
        "关闭窗口时展示淘汰不得以空驻留索引覆盖 legacy 分页区间"
    );
}

#[test]
#[serial]
fn test_display_retention_disabled_window_keeps_units_inside_legacy_spans() {
    use crate::kit::history_paging::{PageEntry, merge_page, with_spans};
    use crate::kit::tui_render_unit::{TuiNoteLevel, TuiSystemNote};

    crate::kit::history_paging::reset_browsing_state();
    let mut state = make_fold_test_state();
    state.active_session_id = "retention-legacy-span".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::default());
    assert!(!state.window.is_enabled());
    let unit = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "page-note".into(),
        level: TuiNoteLevel::Info,
        content_hash: 0,
    });
    let page = PageEntry {
        entry_id: "legacy:page".into(),
        position: 1000,
        units: vec![unit.clone(), unit],
    };
    with_spans(|spans| {
        merge_page(&mut state.committed, spans, &[page]);
    });
    assert_eq!(state.committed.len(), 2);
    for index in 0..MAX_ENTRIES + 64 {
        note(&mut state, format!("outside-{index}"));
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    }
    assert_eq!(
        crate::kit::history_paging::legacy_slot_ranges(),
        vec![(0, 2)],
        "多单元页面条目的区间不得被单点淘汰截断"
    );
    assert_eq!(state.committed.len(), 2 + MAX_ENTRIES);
    assert!(
        matches!(&state.committed[0], TuiRenderUnit::TuiSystemNote(note) if note.text == "page-note")
    );
}

#[test]
#[serial]
fn test_display_retention_keeps_latest_oversized_notice_readable_and_bounds_older_records() {
    let mut state = state();
    for index in 0..3 {
        note(&mut state, format!("{index}{}", "x".repeat(MAX_BYTES + 1)));
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    }
    assert_eq!(state.committed.len(), 1);
    assert!(
        matches!(&state.committed[0], TuiRenderUnit::TuiSystemNote(note) if note.text.starts_with('2'))
    );
    note(&mut state, "new small notice".into());
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(retained(&state) <= MAX_BYTES);
}
