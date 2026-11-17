//! Archive publication regression scenarios.

use super::AcpEventData;
use super::TuiRenderUnit;
use super::WindowIdentity;
use super::WindowPagePlacement;
use super::WindowRequestContext;
use super::context;

#[test]
#[serial]
fn page_merge_keeps_live_current_turn_untouched() {
    let (mut state, _guard) = super::state_with_window();
    state
        .current_turn
        .append_text("streaming live", Some("m-live"));
    super::load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![super::text_item("m1", 10, "one")],
    );
    super::load_page(
        &mut state,
        "r2",
        WindowPagePlacement::BeforeFirst,
        vec![super::text_item("m0", 5, "zero")],
    );
    assert_eq!(super::committed_texts(&state), vec!["zero", "one"]);
    assert_eq!(
        state.window.displayed_entry_count(),
        2,
        "live region is not in the window display ledger"
    );
    assert!(!state.current_turn.is_empty());
    assert!(
        state.current_turn.text.contains("streaming live"),
        "window page merge must not flush or rewrite current_turn"
    );
}

#[test]
#[serial]
fn duplicate_persistent_identity_is_not_displayed_twice() {
    let (mut state, _guard) = super::state_with_window();
    super::load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![super::text_item("m1", 10, "one")],
    );
    super::load_page(
        &mut state,
        "r2",
        WindowPagePlacement::Tail,
        vec![super::text_item("m1", 10, "one")],
    );
    assert_eq!(super::committed_texts(&state), vec!["one"]);
    assert_eq!(state.window.displayed_entry_count(), 1);
    assert_eq!(state.window.segment_count(), 1);
}

#[test]
#[serial]
fn open_with_live_region_does_not_clear_or_merge_display() {
    let (mut state, _guard) = super::state_with_window();
    state
        .committed
        .push_back(crate::kit::tui_render_unit::TuiRenderUnit::TuiUserBubble(
            crate::kit::tui_render_unit::TuiUserBubble::new("live committed".to_string()),
        ));
    state
        .current_turn
        .append_text("streaming live", Some("m-live"));
    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let context = WindowRequestContext {
        request_id: "r-open-live".into(),
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "", 0, ""),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation: state.generation,
        gap: None,
    };
    super::dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(super::response(
                identity,
                vec![super::text_item("m1", 10, "page")],
            )),
        },
    );
    assert_eq!(super::committed_texts(&state), vec!["live committed"]);
    assert!(!state.current_turn.is_empty());
    assert_eq!(
        state.window.segment_count(),
        0,
        "live 区域非空时 open 页不接纳，留待后续翻页重新获取"
    );
    assert_eq!(state.window.displayed_entry_count(), 0);
}

#[test]
#[serial]
fn open_without_live_region_adopts_window_page_and_clears_replay() {
    let (mut state, _guard) = super::state_with_window();
    state
        .committed
        .push_back(crate::kit::tui_render_unit::TuiRenderUnit::TuiUserBubble(
            crate::kit::tui_render_unit::TuiUserBubble::new("legacy replay".to_string()),
        ));
    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let context = WindowRequestContext {
        request_id: "r-open".into(),
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "", 0, ""),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation: state.generation,
        gap: None,
    };
    super::dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(super::response(
                identity,
                vec![super::text_item("m1", 10, "page")],
            )),
        },
    );
    assert_eq!(super::committed_texts(&state), vec!["page"]);
    assert_eq!(state.window.displayed_entry_count(), 1);
}

#[test]
#[serial]
fn accepted_page_promotes_live_archive_without_duplicate_body() {
    use crate::kit::history_window::{
        CommitOutcome, HistoryCommitInfo, PersistentEntryKey, PinReason, SegmentOrigin,
    };
    use peri_acp_types::history_window::HistoryCommittedTail;
    use peri_acp_types::messages::MessageId;
    use peri_acp_types::thread::ThreadId;

    let (mut state, _guard) = super::state_with_window();
    state.current_turn.append_text("live body", Some("live"));
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = PersistentEntryKey::own("s1:live");
    let segment_key = state.window.index().locate_entry(&key).expect("accepted page promotes live archive without duplicate body: the index fixture operation uses valid inputs").segment;
    let segment = state.window.segment(segment_key).expect("accepted page promotes live archive without duplicate body: the segment fixture operation uses valid inputs");
    assert_eq!(segment.origin, SegmentOrigin::LiveSeal);
    assert_eq!(segment.entries[0].position, None);
    assert!(!segment.durable);
    assert!(segment.pins.contains(PinReason::Unpersisted));
    let identity = state.window.identity().cloned().expect("accepted page promotes live archive without duplicate body: the identity fixture operation uses valid inputs");
    let context = context("live-page", state.window.generation());
    let page = super::response(identity, vec![super::text_item("live", 10, "live body")]);
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );
    assert_eq!(state.committed.len(), 1);
    assert_eq!(state.window.segment_count(), 1);
    assert_eq!(
        state.window.segment(segment_key).expect("accepted page promotes live archive without duplicate body: the segment fixture operation uses valid inputs").entries[0].position,
        Some(10)
    );
    let info = HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-1".into(),
        structure_version: 7,
        checkpoint_id: "live-cp".into(),
        summary_entry_id: None,
        committed_tail: HistoryCommittedTail::new(
            &ThreadId::from("s1".to_string()),
            MessageId::new(),
            10,
        )
        .encode(),
    };
    assert_eq!(
        state.window.apply_commit(&info),
        CommitOutcome::Applied { sealed_segments: 1 }
    );
    let segment = state.window.segment(segment_key).expect("accepted page promotes live archive without duplicate body: the segment fixture operation uses valid inputs");
    assert!(segment.sealed && segment.durable);
    assert!(!segment.pins.contains(PinReason::Unpersisted));
}

#[test]
#[serial]
fn accepted_page_with_commit_receipt_seals_immediately() {
    use crate::kit::history_window::{PersistentEntryKey, PinReason};
    use peri_acp_types::history_window::HistoryCommittedTail;
    use peri_acp_types::messages::MessageId;
    use peri_acp_types::thread::ThreadId;

    let (mut state, _guard) = super::state_with_window();
    state.current_turn.append_text("live body", Some("live"));
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = PersistentEntryKey::own("s1:live");
    let segment_key = state.window.index().locate_entry(&key).expect("accepted page with commit receipt seals immediately: the index fixture operation uses valid inputs").segment;
    let identity = state.window.identity().cloned().expect("accepted page with commit receipt seals immediately: the identity fixture operation uses valid inputs");
    let context = context("live-page-commit", state.window.generation());
    let mut page = super::response(identity, vec![super::text_item("live", 10, "live body")]);
    // 页响应自带持久回执：接纳后应立即封印，而不是等下一次 Full Compact。
    page.committed_tail = Some(
        HistoryCommittedTail::new(&ThreadId::from("s1".to_string()), MessageId::new(), 10).encode(),
    );
    page.checkpoint_id = Some("live-page-cp".into());
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );
    let segment = state.window.segment(segment_key).expect("accepted page with commit receipt seals immediately: the segment fixture operation uses valid inputs");
    assert!(
        segment.sealed,
        "page receipt must seal promoted live segment"
    );
    assert!(segment.durable);
    assert!(!segment.pins.contains(PinReason::Unpersisted));
}

#[test]
#[serial]
fn rejected_page_does_not_promote_live_archive() {
    let (mut state, _guard) = super::state_with_window();
    state.current_turn.append_text("live body", Some("live"));
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = crate::kit::history_window::PersistentEntryKey::own("s1:live");
    let segment_key = state.window.index().locate_entry(&key).expect("rejected page does not promote live archive: the index fixture operation uses valid inputs").segment;
    let identity = state.window.identity().cloned().expect("rejected page does not promote live archive: the identity fixture operation uses valid inputs");
    // 无法定位的 gap 在 admit_page 阶段拒绝，不能预先提升响应中的 live 身份。
    let mut context = context("bad-gap", state.window.generation());
    context.gap = Some(crate::kit::history_window::GapId(999));
    let page = super::response(identity, vec![super::text_item("live", 10, "live body")]);
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );
    assert_eq!(
        state.window.segment(segment_key).expect("rejected page does not promote live archive: the segment fixture operation uses valid inputs").entries[0].position,
        None
    );
    assert_eq!(state.committed.len(), 1);
}

#[test]
#[serial]
fn reopen_keeps_live_archive_outside_new_page() {
    let (mut state, _guard) = super::state_with_window();
    state.current_turn.append_text("live body", Some("live"));
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let mut identity = state.window.identity().cloned().expect("reopen keeps live archive outside new page: the identity fixture operation uses valid inputs");
    identity.snapshot_token = "snap-2".into();
    let mut context = context("reopen-live", state.window.generation());
    context.is_open = true;
    context.display_generation = state.generation;
    let page = super::response(identity, vec![super::text_item("older", 5, "older body")]);
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );
    assert_eq!(state.committed.len(), 2);
    assert!(matches!(
        &state.committed[1],
        TuiRenderUnit::TuiAssistantBubble(b) if b.text == "live body"
    ));
    let key = crate::kit::history_window::PersistentEntryKey::own("s1:live");
    assert!(state.window.index().contains_entry(&key));
    assert_eq!(state.window.displayed_range(&key), Some((1, 1)));
}

use serial_test::serial;
