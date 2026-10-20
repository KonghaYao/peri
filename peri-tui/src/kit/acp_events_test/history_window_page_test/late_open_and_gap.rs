//! 迟到 open 与 gap seek 的窗口页回归（自 `history_window_page_test` 拆分）。

use super::*;

#[test]
#[serial]
fn late_open_replaces_only_replay_prefix_and_keeps_appended_turn() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    crate::kit::history_paging::reset_browsing_state();
    // 旧回放：一个已登记区间的展示单元。
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "legacy replay".to_string(),
        )));
    crate::kit::history_paging::mark_own_entry("s1:replay", 10);
    crate::kit::history_paging::note_own_entry_unit(0);
    crate::kit::history_paging::close_own_entry();

    let requested_display = state.generation;
    // open 在途期间：新用户气泡进入 committed 并发布。
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "new question".to_string(),
        )));
    push_view_models(&mut state);
    assert!(state.generation > requested_display);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let context = WindowRequestContext {
        request_id: "r-open-late".into(),
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "", 0, ""),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation: requested_display,
        gap: None,
    };
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response(identity, vec![text_item("m1", 10, "page")])),
        },
    );

    assert_eq!(
        committed_texts(&state),
        vec!["page", "new question"],
        "旧回放被原位替换，open 在途期间追加的消息保留且不重复"
    );
    assert_eq!(state.window.segment_count(), 1);
    assert_eq!(state.window.displayed_entry_count(), 1);
}

#[test]
#[serial]
fn late_open_without_known_replay_boundary_keeps_display_intact() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    crate::kit::history_paging::reset_browsing_state();
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "legacy replay".to_string(),
        )));
    let requested_display = state.generation;
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "new answer".to_string(),
        )));
    push_view_models(&mut state);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let context = WindowRequestContext {
        request_id: "r-open-late-unknown".into(),
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "", 0, ""),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation: requested_display,
        gap: None,
    };
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response(identity, vec![text_item("m1", 10, "page")])),
        },
    );

    assert_eq!(
        committed_texts(&state),
        vec!["legacy replay", "new answer"],
        "无法确认旧回放边界时不得整体清空，也不得把旧页重复铺上去"
    );
    assert_eq!(
        state.window.segment_count(),
        0,
        "无法安全合并的页面不预留、不接纳，留待后续翻页重新获取"
    );
    assert_eq!(state.window.displayed_entry_count(), 0);
}

#[test]
#[serial]
fn gap_seek_response_fills_the_gap_in_place_without_reordering() {
    use crate::kit::history_window::{
        HistoryEntryView, PersistentEntryKey, ReleasePlan, SegmentDraft, SegmentPlacement,
        WindowAnchor, WindowAnchorOffset,
    };
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    let identity = state.window.identity().cloned().expect("identity");
    let source = crate::kit::history_window::HistorySource::Own;
    let entry_a = HistoryEntryView::new(
        PersistentEntryKey::own("s1:a"),
        Some(10),
        1,
        10,
        20,
        Vec::new(),
    );
    let key_a = state
        .window
        .register_segment(
            SegmentDraft::new(source.clone(), vec![entry_a], 10, 20),
            SegmentPlacement::Tail,
        )
        .expect("segment a");
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "a".to_string(),
        )));

    let entry_b = HistoryEntryView::new(
        PersistentEntryKey::own("s1:b"),
        Some(20),
        1,
        10,
        20,
        Vec::new(),
    );
    let key_b = state
        .window
        .register_segment(
            SegmentDraft::new(source, vec![entry_b.clone()], 10, 20),
            SegmentPlacement::Tail,
        )
        .expect("segment b");
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "b".to_string(),
        )));

    // 桥接插入正文时同步登记展示账本；模型索引不代表 committed 的真实槽位。
    state.window.mark_display_block(
        &[
            (PersistentEntryKey::own("s1:a"), 1),
            (entry_b.key.clone(), 1),
        ],
        0,
    );
    let (before, after) = state.window.neighbor_entries(key_b);
    let segment = state.window.segment(key_b).expect("segment b").clone();
    let plan = ReleasePlan {
        segment: key_b,
        source: segment.source.clone(),
        entries: segment.entry_keys(),
        unit_count: segment.unit_count,
        body_bytes: segment.body_bytes,
        reserved_bytes: segment.reserved_bytes,
        slot_range: state.window.index().slot_range(key_b),
        before,
        after,
        before_cursor: Some("before-b".into()),
        after_cursor: Some("after-b".into()),
    };
    let released = crate::kit::history_window::release_window_plans(
        &mut state,
        vec![plan],
        crate::kit::history_window::ReleaseReason::BudgetPressure,
    );
    assert_eq!(released.released_units, 1);
    // 缺口之后还有 live 追加内容：补缺口必须插在 live 之前，而不是追加到尾部。
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "live answer".to_string(),
        )));
    state.window.set_anchor(Some(WindowAnchor::new(
        entry_b.key.clone(),
        None,
        WindowAnchorOffset::CharOffset(0),
    )));
    assert_eq!(state.window.gap_count(), 1);

    let (context, anchor) =
        match crate::kit::history_page_consumer::build_window_gap_request(&state.window)
            .expect("gap request")
        {
            crate::kit::history_page_consumer::WindowConsumerRequest::Seek {
                context,
                anchor,
                ..
            } => (context, anchor),
            other => panic!("expected gap seek, got {other:?}"),
        };
    assert_eq!(anchor, entry_b.key);
    assert_eq!(
        context.gap,
        state.window.gaps().keys().next().copied(),
        "gap identity must travel with the request"
    );
    assert!(
        key_a != key_b,
        "test fixture must keep two distinct resident segments"
    );

    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response(identity, vec![text_item("b", 20, "b")])),
        },
    );

    assert_eq!(
        committed_texts(&state),
        vec!["a", "b", "live answer"],
        "gap response must be inserted where the gap was, before live tail content"
    );
    assert_eq!(state.window.gap_count(), 0, "filled gap must be removed");
    assert_eq!(state.window.segment_count(), 2);
}
