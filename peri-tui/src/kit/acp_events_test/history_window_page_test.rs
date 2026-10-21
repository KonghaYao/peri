//! S5.4 桥接页接纳、预留结算与迟到页丢弃。

use super::*;
use crate::kit::history_page_consumer::{self, WindowPagePlacement, WindowRequestContext};
use crate::kit::history_paging::{
    WindowBodyDescriptor, WindowPageItem, WindowPageResponse, WindowSubItem,
};
use crate::kit::history_window::{HistoryWindow, HistoryWindowConfig, WindowIdentity};
use serial_test::serial;

#[path = "history_window_page_test/late_open_and_gap.rs"]
mod late_open_and_gap;
#[path = "history_window_page_test/reopen_identity.rs"]
mod reopen_identity;

struct WindowAtomsGuard;

impl Drop for WindowAtomsGuard {
    fn drop(&mut self) {
        crate::kit::atoms::HISTORY_WINDOW_V1.set(false);
        crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(false);
        history_page_consumer::reset_window_paging_state();
        crate::kit::history_window::clear_window_viewport();
    }
}

fn setup_atoms(identity: &WindowIdentity) -> WindowAtomsGuard {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::HISTORY_WINDOW_V1.set(true);
    crate::kit::atoms::HISTORY_WINDOW_CAPABLE.set(true);
    history_page_consumer::reset_window_paging_state();
    history_page_consumer::note_window_generation(1);
    let handle = crate::kit::atoms::HISTORY_WINDOW_PAGING.state();
    let mut state = handle.write();
    state.session_id = identity.session_id.clone();
    state.identity = Some(identity.clone());
    state.window_generation = 1;
    state.read_ticket = Some("ticket-1".into());
    WindowAtomsGuard
}

fn state_with_window() -> (BridgeState, WindowAtomsGuard) {
    let identity = WindowIdentity::new("s1", "gen-1", 7, "snap-1");
    let guard = setup_atoms(&identity);
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state.window.begin_window(identity);
    state.phase = SessionPhase::PromptRunning;
    state.current_request_id = Some("req-1".into());
    state.turn_generation = 9;
    (state, guard)
}

fn response(identity: WindowIdentity, items: Vec<WindowPageItem>) -> WindowPageResponse {
    WindowPageResponse {
        session_id: identity.session_id.clone(),
        identity,
        committed_tail: None,
        checkpoint_id: None,
        read_ticket: "ticket-1".into(),
        before_cursor: Some("before-2".into()),
        after_cursor: Some("after-2".into()),
        items,
        body_descriptors: Vec::new(),
        descriptor_only_items: 0,
        inherited: None,
        inherited_unsupported: false,
        reopen_required: false,
    }
}

fn item(id: &str, position: Option<u64>, descriptor_only: bool) -> WindowPageItem {
    if descriptor_only {
        return WindowPageItem {
            source: crate::kit::history_window::HistorySource::Own,
            entry_id: format!("s1:{id}"),
            position,
            oversize: true,
            sub_items: Vec::new(),
            descriptor: None,
            descriptor_only: true,
        };
    }
    text_item(id, position.unwrap_or(0), "text")
}

fn text_item(id: &str, position: u64, text: &str) -> WindowPageItem {
    WindowPageItem {
        source: crate::kit::history_window::HistorySource::Own,
        entry_id: format!("s1:{id}"),
        position: Some(position),
        oversize: false,
        sub_items: vec![WindowSubItem {
            position: 0,
            update: serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text },
                "messageId": id,
            }),
            system_reminder: false,
        }],
        descriptor: None,
        descriptor_only: false,
    }
}

fn inherited_source() -> crate::kit::history_window::HistorySource {
    crate::kit::history_window::HistorySource::inherited_with_ancestor(
        "snap-frozen",
        Some("ancestor-session".into()),
    )
}

fn inherited_text_item(id: &str, text: &str) -> WindowPageItem {
    WindowPageItem {
        source: inherited_source(),
        entry_id: format!("s1:{id}"),
        position: None,
        oversize: false,
        sub_items: vec![WindowSubItem {
            position: 0,
            update: serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text},
                "messageId": id,
            }),
            system_reminder: false,
        }],
        descriptor: None,
        descriptor_only: false,
    }
}

fn oversize_item(id: &str, position: u64, total: u64) -> WindowPageItem {
    WindowPageItem {
        source: crate::kit::history_window::HistorySource::Own,
        entry_id: format!("s1:{id}"),
        position: Some(position),
        oversize: true,
        sub_items: Vec::new(),
        descriptor: Some(WindowBodyDescriptor {
            entry_id: format!("s1:{id}"),
            source: Some(crate::kit::history_window::HistorySource::Own),
            total_bytes: total,
            chunk_size: 64 * 1024,
            read_ticket: "ticket-1".into(),
            snapshot_token: Some("snap-1".into()),
        }),
        descriptor_only: false,
    }
}

fn context(request_id: &str, generation: u64) -> WindowRequestContext {
    context_at(request_id, generation, WindowPagePlacement::Tail)
}

fn context_at(
    request_id: &str,
    generation: u64,
    placement: WindowPagePlacement,
) -> WindowRequestContext {
    WindowRequestContext {
        request_id: request_id.into(),
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "gen-1", 7, "snap-1"),
        window_generation: generation,
        reserved_bytes: 1024,
        placement,
        is_open: false,
        display_generation: 0,
        gap: None,
    }
}

#[test]
#[serial]
fn window_page_loaded_admits_segments_and_settles_after_publication() {
    let (mut state, _guard) = state_with_window();
    let identity = state.window.identity().cloned().expect("identity");
    let context = context("req-page", state.window.generation());
    let response = response(identity, vec![item("m1", Some(10), false)]);
    let intent = dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context.clone()),
            response: Box::new(response),
        },
    );
    assert_eq!(intent, PublicationIntent::Deferred);
    assert_eq!(state.window.segment_count(), 1);
    assert_eq!(
        state.committed.len(),
        1,
        "page units enter the display stream"
    );
    assert_eq!(state.window.displayed_entry_count(), 1);
    assert!(
        state
            .window
            .is_entry_displayed(&crate::kit::history_window::PersistentEntryKey::own(
                "s1:m1"
            ))
    );
    assert!(state.window.pending_merge_request_id().is_some());
    assert!(
        state
            .window
            .segment(*state.window.segments().keys().next().unwrap())
            .unwrap()
            .entries[0]
            .position
            .is_some()
    );

    push_view_models(&mut state);
    assert!(state.window.pending_merge_request_id().is_none());
    let consumer = crate::kit::atoms::HISTORY_WINDOW_PAGING
        .state()
        .read()
        .clone();
    assert!(consumer.pending_merge.is_none());
    assert_eq!(consumer.reserved_bytes, 0);
    assert_eq!(state.phase, SessionPhase::PromptRunning);
    assert_eq!(state.current_request_id.as_deref(), Some("req-1"));
    assert_eq!(state.turn_generation, 9);
}

#[test]
#[serial]
fn descriptor_only_page_becomes_explicit_placeholder_entry() {
    let (mut state, _guard) = state_with_window();
    let identity = state.window.identity().cloned().expect("identity");
    let context = context("req-descriptor", state.window.generation());
    let response = response(identity, vec![item("big", None, true)]);
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );
    let segment = state
        .window
        .segment(*state.window.segments().keys().next().expect("segment"))
        .expect("segment");
    assert_eq!(segment.entries.len(), 1);
    assert_eq!(segment.entries[0].position, None);
    assert_eq!(segment.entries[0].unit_count, 0);
    assert_eq!(segment.entries[0].body_bytes, 0);
    assert_eq!(segment.body_bytes, 0);
    assert!(state.committed.is_empty());
}

#[test]
#[serial]
fn stale_generation_page_is_rejected_without_admission() {
    let (mut state, _guard) = state_with_window();
    let identity = state.window.identity().cloned().expect("identity");
    let context = context("req-stale", state.window.generation().saturating_add(1));
    let response = response(identity, vec![item("m1", Some(10), false)]);
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );
    assert_eq!(state.window.segment_count(), 0);
    let consumer = crate::kit::atoms::HISTORY_WINDOW_PAGING
        .state()
        .read()
        .clone();
    assert!(
        !consumer.is_degraded(),
        "a late page must not degrade the live window"
    );
    assert_eq!(consumer.reserved_bytes, 0);
}

#[test]
#[serial]
fn open_page_begins_window_and_aligns_generation() {
    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-new");
    let _guard = setup_atoms(&identity);
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    let context = WindowRequestContext {
        request_id: "req-open".into(),
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "", 0, ""),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation: state.generation,
        gap: None,
    };
    let response = response(identity.clone(), vec![item("m1", Some(10), false)]);
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );
    assert_eq!(state.window.identity(), Some(&identity));
    assert!(state.window.generation() > 0);
    assert_eq!(state.window.segment_count(), 1);
    assert_eq!(state.committed.len(), 1);
    assert_eq!(
        crate::kit::atoms::HISTORY_WINDOW_GENERATION.get(),
        state.window.generation()
    );
}

#[test]
#[serial]
fn degraded_event_stops_bridge_eviction_but_keeps_segments() {
    let (mut state, _guard) = state_with_window();
    let identity = state.window.identity().cloned().expect("identity");
    let context = context("req-page", state.window.generation());
    let response = response(identity, vec![item("m1", Some(10), false)]);
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );
    push_view_models(&mut state);
    let before = state.window.segment_count();

    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowDegraded {
            session_id: "s1".into(),
            reason: "cursor expired".into(),
        },
    );
    assert!(state.window.is_degraded());
    assert_eq!(state.window.segment_count(), before);
    assert!(state.window.plan_eviction(10_000).plans.is_empty());
    assert!(matches!(
        state.window.reserve_page("after-degraded", 100),
        crate::kit::history_window::ReserveOutcome::Rejected(
            crate::kit::history_window::ReserveRejection::Degraded { .. }
        )
    ));
}

fn load_page(
    state: &mut BridgeState,
    request_id: &str,
    placement: WindowPagePlacement,
    items: Vec<WindowPageItem>,
) {
    let identity = state.window.identity().cloned().expect("identity");
    let context = context_at(request_id, state.window.generation(), placement);
    dispatch_for_bridge(
        state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response(identity, items)),
        },
    );
    push_view_models(state);
}

fn committed_texts(state: &BridgeState) -> Vec<String> {
    state
        .committed
        .iter()
        .map(|unit| match unit {
            crate::kit::tui_render_unit::TuiRenderUnit::TuiAssistantBubble(bubble) => {
                bubble.text.clone()
            }
            crate::kit::tui_render_unit::TuiRenderUnit::TuiUserBubble(bubble) => {
                bubble.text.clone()
            }
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
#[serial]
fn inherited_items_keep_source_and_are_pinned() {
    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r-own",
        WindowPagePlacement::Tail,
        vec![text_item("own", 10, "own")],
    );
    load_page(
        &mut state,
        "r-inherited",
        WindowPagePlacement::BeforeFirst,
        vec![inherited_text_item("inh", "frozen")],
    );
    assert_eq!(committed_texts(&state), vec!["frozen", "own"]);
    assert_eq!(
        state.window.segment_count(),
        2,
        "own/inherited stay in separate segments"
    );
    let inherited_key =
        crate::kit::history_window::PersistentEntryKey::new(inherited_source(), "s1:inh");
    assert!(state.window.is_entry_displayed(&inherited_key));
    let segment = state
        .window
        .segments()
        .values()
        .find(|segment| {
            segment
                .entries
                .iter()
                .any(|entry| entry.key == inherited_key)
        })
        .expect("inherited segment");
    assert!(
        segment
            .pins
            .contains(crate::kit::history_window::PinReason::InheritedUnsupported)
    );
    let plan = state.window.plan_eviction(100_000);
    assert!(
        plan.plans
            .iter()
            .all(|plan| plan.source == crate::kit::history_window::HistorySource::Own),
        "inherited segments remain pinned"
    );
}

#[test]
#[serial]
fn inherited_unsupported_response_surfaces_explicit_notice() {
    let (mut state, _guard) = state_with_window();
    let identity = state.window.identity().cloned().expect("identity");
    let mut response = response(identity, vec![inherited_text_item("inh", "frozen")]);
    response.inherited_unsupported = true;
    response.inherited = Some(crate::kit::history_paging::WindowInheritedSnapshot {
        snapshot_id: "snap-frozen".into(),
        ancestor_thread_id: Some("ancestor-session".into()),
        first_entry_id: Some("s1:inh".into()),
        last_entry_id: Some("s1:inh".into()),
        entry_count: 1,
        supported: false,
    });
    let context = context("r-inherited-unsupported", state.window.generation());
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );
    assert!(
        crate::kit::atoms::HISTORY_INHERITED_NOTICE
            .state()
            .read()
            .is_some()
    );
    assert!(crate::kit::atoms::NOTIFICATION.state().read().is_some());
    crate::kit::atoms::HISTORY_INHERITED_NOTICE.set(None);
    crate::kit::atoms::NOTIFICATION.set(None);
}

#[test]
#[serial]
fn inherited_oversize_descriptor_uses_frozen_source_for_detail() {
    let (mut state, _guard) = state_with_window();
    let mut item = oversize_item("frozen-big", 20, 128 * 1024);
    item.source = inherited_source();
    item.entry_id = "s1:frozen-big".into();
    if let Some(descriptor) = item.descriptor.as_mut() {
        descriptor.entry_id = "s1:frozen-big".into();
        descriptor.source = Some(inherited_source());
        descriptor.snapshot_token = Some("snap-window".into());
    }
    load_page(
        &mut state,
        "r-inherited-big",
        WindowPagePlacement::BeforeFirst,
        vec![item],
    );
    let key =
        crate::kit::history_window::PersistentEntryKey::new(inherited_source(), "s1:frozen-big");
    let descriptor = state
        .window
        .detail_descriptor(&key)
        .expect("inherited descriptor")
        .clone();
    assert_eq!(descriptor.key.source, inherited_source());
    let mut detail =
        crate::kit::panels::history_detail::HistoryDetailWindow::open(descriptor.clone())
            .expect("detail");
    let request = detail.request(0, 64 * 1024).expect("request");
    assert_eq!(request.source, inherited_source());
    assert_eq!(request.snapshot_token, "snap-window");
    assert_eq!(request.entry_id, "s1:frozen-big");

    // own 侧后续变化不改变 inherited descriptor 的冻结快照绑定。
    load_page(
        &mut state,
        "r-own-after",
        WindowPagePlacement::Tail,
        vec![text_item("own-after", 30, "own later")],
    );
    let after = state
        .window
        .detail_descriptor(&key)
        .expect("descriptor survives own changes");
    assert_eq!(after, &descriptor);
}

#[test]
#[serial]
fn oversize_only_page_renders_placeholder_and_retains_descriptor() {
    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r-oversize",
        WindowPagePlacement::Tail,
        vec![oversize_item("big", 10, 64 * 1024)],
    );
    assert_eq!(state.committed.len(), 1);
    let crate::kit::tui_render_unit::TuiRenderUnit::TuiSystemNote(note) = &state.committed[0]
    else {
        panic!("expected oversize placeholder note");
    };
    assert!(note.text.contains("超大内容"));
    let key = crate::kit::history_window::PersistentEntryKey::own("s1:big");
    let descriptor = state.window.detail_descriptor(&key).expect("descriptor");
    assert_eq!(descriptor.total_bytes, 64 * 1024);
    assert_eq!(descriptor.key, key);
    assert_eq!(state.window.displayed_entry_count(), 1);
    crate::kit::panels::history_detail::open_history_detail(&state.window, &key)
        .expect("open detail");
    assert!(crate::kit::atoms::HISTORY_DETAIL.state().read().is_some());
    crate::kit::panels::history_detail::close_history_detail();
    assert!(crate::kit::atoms::HISTORY_DETAIL.state().read().is_none());
    assert_eq!(
        state.committed.len(),
        1,
        "closing detail keeps the placeholder"
    );
}

#[test]
#[serial]
fn before_and_after_pages_insert_at_segment_boundaries() {
    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r-mid",
        WindowPagePlacement::Tail,
        vec![text_item("m20", 20, "twenty")],
    );
    load_page(
        &mut state,
        "r-before",
        WindowPagePlacement::BeforeFirst,
        vec![text_item("m10", 10, "ten")],
    );
    load_page(
        &mut state,
        "r-after",
        WindowPagePlacement::Tail,
        vec![text_item("m30", 30, "thirty")],
    );
    assert_eq!(committed_texts(&state), vec!["ten", "twenty", "thirty"]);
    assert_eq!(state.window.segment_count(), 3);
    assert_eq!(state.window.displayed_entry_count(), 3);
    let spans = crate::kit::history_paging::HistorySpans::from_resident_window(&state.window);
    assert_eq!(spans.slots(), vec![(0, 1), (1, 1), (2, 1)]);
}

#[test]
#[serial]
fn viewport_anchor_sync_pins_resident_segment_before_admission() {
    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );
    let key = crate::kit::history_window::PersistentEntryKey::own("s1:m1");
    crate::kit::atoms::WINDOW_VIEWPORT.set(Some(crate::kit::history_window::WindowViewport {
        anchor: Some(crate::kit::history_window::WindowAnchor::new(
            key.clone(),
            None,
            crate::kit::history_window::WindowAnchorOffset::CharOffset(0),
        )),
        ..Default::default()
    }));
    load_page(
        &mut state,
        "r-before",
        WindowPagePlacement::BeforeFirst,
        vec![text_item("m0", 5, "zero")],
    );
    assert_eq!(
        state.window.anchor().map(|anchor| anchor.entry.clone()),
        Some(key.clone())
    );
    let segment = state
        .window
        .segments()
        .values()
        .find(|segment| segment.entries.iter().any(|entry| entry.key == key))
        .expect("anchored segment");
    assert!(
        segment
            .pins
            .contains(crate::kit::history_window::PinReason::Viewport)
    );
    assert_eq!(committed_texts(&state), vec!["zero", "one"]);
}

#[test]
#[serial]
fn page_merge_keeps_live_current_turn_untouched() {
    let (mut state, _guard) = state_with_window();
    state
        .current_turn
        .append_text("streaming live", Some("m-live"));
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );
    load_page(
        &mut state,
        "r2",
        WindowPagePlacement::BeforeFirst,
        vec![text_item("m0", 5, "zero")],
    );
    assert_eq!(committed_texts(&state), vec!["zero", "one"]);
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
    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );
    load_page(
        &mut state,
        "r2",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );
    assert_eq!(committed_texts(&state), vec!["one"]);
    assert_eq!(state.window.displayed_entry_count(), 1);
    assert_eq!(state.window.segment_count(), 1);
}

#[test]
#[serial]
fn open_with_live_region_does_not_clear_or_merge_display() {
    let (mut state, _guard) = state_with_window();
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
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response(identity, vec![text_item("m1", 10, "page")])),
        },
    );
    assert_eq!(committed_texts(&state), vec!["live committed"]);
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
    let (mut state, _guard) = state_with_window();
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
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response(identity, vec![text_item("m1", 10, "page")])),
        },
    );
    assert_eq!(committed_texts(&state), vec!["page"]);
    assert_eq!(state.window.displayed_entry_count(), 1);
}

#[test]
#[serial]
fn rewind_completed_resets_window_and_consumer_reservation() {
    let (mut state, _guard) = state_with_window();
    let identity = state.window.identity().cloned().expect("identity");
    let context = context("req-rewind", state.window.generation());
    let response = response(identity, vec![item("m1", Some(10), false)]);
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );
    assert!(state.window.pending_merge_request_id().is_some());
    crate::kit::atoms::HISTORY_DETAIL.set(Some(
        crate::kit::panels::history_detail::HistoryDetailWindow::open(
            crate::kit::history_window::DetailDescriptor::new(
                crate::kit::history_window::HistorySource::Own,
                "s1:m1",
                1024,
                1024,
                "snap-1",
                "ticket-1",
            ),
        )
        .expect("detail"),
    ));

    dispatch_for_bridge(
        &mut state,
        &AcpEventData::RewindCompleted {
            messages_json: "[]".into(),
        },
    );
    assert_eq!(state.window.segment_count(), 0);
    assert!(state.window.identity().is_none());
    let consumer = crate::kit::atoms::HISTORY_WINDOW_PAGING
        .state()
        .read()
        .clone();
    assert!(consumer.in_flight.is_none());
    assert!(consumer.pending_merge.is_none());
    assert_eq!(consumer.reserved_bytes, 0);
}

#[test]
#[serial]
fn empty_page_accepts_without_pending_merge() {
    let (mut state, _guard) = state_with_window();
    let identity = state.window.identity().cloned().expect("identity");
    let context = context("req-empty", state.window.generation());
    let response = response(identity, Vec::new());
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );
    assert_eq!(state.window.segment_count(), 0);
    assert!(state.window.pending_merge_request_id().is_none());
    let consumer = crate::kit::atoms::HISTORY_WINDOW_PAGING
        .state()
        .read()
        .clone();
    assert_eq!(consumer.reserved_bytes, 0);
    assert!(!consumer.is_degraded());
}

#[test]
#[serial]
fn accepted_page_promotes_live_archive_without_duplicate_body() {
    use crate::kit::history_window::{
        CommitOutcome, HistoryCommitInfo, PersistentEntryKey, PinReason, SegmentOrigin,
    };
    use peri_acp_types::{
        history_window::HistoryCommittedTail, messages::MessageId, thread::ThreadId,
    };

    let (mut state, _guard) = state_with_window();
    state.current_turn.append_text("live body", Some("live"));
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = PersistentEntryKey::own("s1:live");
    let segment_key = state.window.index().locate_entry(&key).unwrap().segment;
    let segment = state.window.segment(segment_key).unwrap();
    assert_eq!(segment.origin, SegmentOrigin::LiveSeal);
    assert_eq!(segment.entries[0].position, None);
    assert!(!segment.durable);
    assert!(segment.pins.contains(PinReason::Unpersisted));
    let identity = state.window.identity().cloned().unwrap();
    let context = context("live-page", state.window.generation());
    let page = response(identity, vec![text_item("live", 10, "live body")]);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );
    assert_eq!(state.committed.len(), 1);
    assert_eq!(state.window.segment_count(), 1);
    assert_eq!(
        state.window.segment(segment_key).unwrap().entries[0].position,
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
    let segment = state.window.segment(segment_key).unwrap();
    assert!(segment.sealed && segment.durable);
    assert!(!segment.pins.contains(PinReason::Unpersisted));
}

#[test]
#[serial]
fn accepted_page_with_commit_receipt_seals_immediately() {
    use crate::kit::history_window::{PersistentEntryKey, PinReason};
    use peri_acp_types::{
        history_window::HistoryCommittedTail, messages::MessageId, thread::ThreadId,
    };

    let (mut state, _guard) = state_with_window();
    state.current_turn.append_text("live body", Some("live"));
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = PersistentEntryKey::own("s1:live");
    let segment_key = state.window.index().locate_entry(&key).unwrap().segment;
    let identity = state.window.identity().cloned().unwrap();
    let context = context("live-page-commit", state.window.generation());
    let mut page = response(identity, vec![text_item("live", 10, "live body")]);
    // 页响应自带持久回执：接纳后应立即封印，而不是等下一次 Full Compact。
    page.committed_tail = Some(
        HistoryCommittedTail::new(&ThreadId::from("s1".to_string()), MessageId::new(), 10).encode(),
    );
    page.checkpoint_id = Some("live-page-cp".into());
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );
    let segment = state.window.segment(segment_key).unwrap();
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
    let (mut state, _guard) = state_with_window();
    state.current_turn.append_text("live body", Some("live"));
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = crate::kit::history_window::PersistentEntryKey::own("s1:live");
    let segment_key = state.window.index().locate_entry(&key).unwrap().segment;
    let identity = state.window.identity().cloned().unwrap();
    // 无法定位的 gap 在 admit_page 阶段拒绝，不能预先提升响应中的 live 身份。
    let mut context = context("bad-gap", state.window.generation());
    context.gap = Some(crate::kit::history_window::GapId(999));
    let page = response(identity, vec![text_item("live", 10, "live body")]);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );
    assert_eq!(
        state.window.segment(segment_key).unwrap().entries[0].position,
        None
    );
    assert_eq!(state.committed.len(), 1);
}

#[test]
#[serial]
fn reopen_keeps_live_archive_outside_new_page() {
    let (mut state, _guard) = state_with_window();
    state.current_turn.append_text("live body", Some("live"));
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let mut identity = state.window.identity().cloned().unwrap();
    identity.snapshot_token = "snap-2".into();
    let mut context = context("reopen-live", state.window.generation());
    context.is_open = true;
    context.display_generation = state.generation;
    let page = response(identity, vec![text_item("older", 5, "older body")]);
    dispatch_and_notify(
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
