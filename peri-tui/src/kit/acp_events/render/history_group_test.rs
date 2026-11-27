use super::TuiRenderUnit;

#[test]
#[serial_test::serial]
fn history_page_rebuilt_process_group_before_todo_keeps_live_working_state() {
    use crate::kit::tui_render_unit::{TuiNoteLevel, TuiSystemNote, TuiTodoSummary};
    crate::kit::atoms::init_atoms();
    super::reset_body_gap_group_cache();
    let note = |n| {
        TuiRenderUnit::TuiSystemNote(TuiSystemNote {
            text: format!("note {n}"),
            level: TuiNoteLevel::Info,
            content_hash: n,
        })
    };
    let raw = im::vector![
        note(1),
        TuiRenderUnit::TuiTodoSummary(TuiTodoSummary::new("tasks".into()))
    ];
    let mut first = raw.clone();
    super::group_body_gaps(
        &mut first,
        &Default::default(),
        &Default::default(),
        true,
        None,
    );
    let mut page = raw;
    page.push_front(note(2));
    super::group_body_gaps(
        &mut page,
        &Default::default(),
        &Default::default(),
        true,
        None,
    );
    assert!(
        matches!(&page[0], TuiRenderUnit::TuiCollapsedGroup(group) if group.is_working && group.count == 2)
    );
    super::reset_body_gap_group_cache();
}

#[test]
#[serial_test::serial]
fn history_page_large_divider_prefix_reuses_group_input_without_retaining_old_messages() {
    use crate::kit::acp_events::{SessionPhase, fold::FoldedHistory};
    use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiUserBubble};
    use std::sync::Arc;
    let mut state = FoldedHistory::default();
    let mut source: im::Vector<TuiRenderUnit> = (0..1200)
        .map(|i| {
            TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("{i}:{}", "x".repeat(6000))))
        })
        .collect();
    let sentinel = Arc::new(TuiAssistantBubble {
        text: "sentinel".into(),
        reasoning: None,
        message_id: Some("sentinel".into()),
        started_at: None,
        duration_ms: None,
        content_hash: 104,
    });
    let sentinel_weak = Arc::downgrade(&sentinel);
    source.push_back(TuiRenderUnit::TuiAssistantBubble(sentinel));
    crate::kit::atoms::init_atoms();
    super::reset_body_gap_group_cache();
    let publish = |state: &mut FoldedHistory, source: &im::Vector<TuiRenderUnit>| {
        let raw = state.project(source, SessionPhase::Idle, &Default::default());
        let (mut input, metadata) = super::decorated_history(&raw, state);
        let snapshot = input.clone();
        super::group_body_gaps(
            &mut input,
            &Default::default(),
            &Default::default(),
            false,
            Some(metadata),
        );
        snapshot
    };
    let first = publish(&mut state, &source);
    source.push_front(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
        "earlier".into(),
    )));
    let next = publish(&mut state, &source);
    assert_eq!(next.len(), first.len() + 2);
    assert!(
        std::ptr::eq(&first[999], &next[1001]),
        "a page larger than the former 6 MiB cutoff must share the actual decorated input"
    );
    let mut cold = source.clone();
    super::insert_turn_dividers(&mut cold, 0, false);
    assert_eq!(next, cold);
    drop((first, next, cold));
    state.invalidate();
    source.clear();
    assert!(
        sentinel_weak.upgrade().is_some(),
        "current grouping owns the last published input"
    );
    super::invalidate_structural_caches();
    assert!(
        sentinel_weak.upgrade().is_none(),
        "the divider marker must not keep released raw or decorated rows alive"
    );
}
