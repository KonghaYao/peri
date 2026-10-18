use std::sync::Arc;

use peri_acp_types::history_window::HistoryBodyChunk;

use super::*;
use crate::kit::history_window::{DetailDescriptor, HistorySource};

fn descriptor(total: u64) -> DetailDescriptor {
    DetailDescriptor::new(
        HistorySource::Own,
        "s1:m1",
        total,
        256 * 1024,
        "snap-1",
        "ticket-1",
    )
}

fn chunk(offset: u64, bytes: &[u8], complete: bool, next: Option<u64>) -> HistoryBodyChunk {
    HistoryBodyChunk {
        entry_id: "s1:m1".into(),
        offset,
        bytes: bytes.to_vec(),
        next_offset: next,
        complete,
    }
}

#[test]
fn detail_chunks_cross_utf8_boundary_without_garble() {
    let mut window = HistoryDetailWindow::open(descriptor(6)).expect("open");
    let text = "中文".as_bytes();
    window
        .apply_chunk(chunk(0, &text[..1], false, Some(1)), 256 * 1024)
        .expect("first byte");
    window
        .apply_chunk(chunk(1, &text[1..], true, None), 256 * 1024)
        .expect("rest");
    assert_eq!(window.loaded_text().expect("utf8"), "中文");
    assert!(window.is_complete());
}

#[test]
fn detail_close_releases_chunk_allocations() {
    let mut window = HistoryDetailWindow::open(descriptor(4)).expect("open");
    window
        .apply_chunk(chunk(0, b"body", true, None), 256 * 1024)
        .expect("chunk");
    let weak = Arc::downgrade(&window.chunk_arc(0).expect("chunk arc"));
    window.close();
    assert_eq!(window.retained_bytes(), 0);
    assert!(weak.upgrade().is_none());
}

#[test]
fn detail_budget_keeps_at_most_4mib() {
    let total = 5 * 1024 * 1024u64;
    let mut window = HistoryDetailWindow::open(descriptor(total)).expect("open");
    let block = vec![b'x'; 256 * 1024];
    let mut offset = 0u64;
    while offset < total {
        let len = block.len().min((total - offset) as usize);
        let complete = offset as usize + len >= total as usize;
        window
            .apply_chunk(
                chunk(
                    offset,
                    &block[..len],
                    complete,
                    (!complete).then_some(offset + len as u64),
                ),
                256 * 1024,
            )
            .expect("bounded chunk");
        offset += len as u64;
    }
    assert!(window.retained_bytes() <= DETAIL_WINDOW_MAX_BYTES);
    assert!(window.chunk_count() > 0);
}

#[test]
fn detail_export_streams_each_chunk_without_full_copy() {
    let mut window = HistoryDetailWindow::open(descriptor(6)).expect("open");
    window
        .apply_chunk(chunk(0, b"abc", false, Some(3)), 256 * 1024)
        .expect("chunk 1");
    window
        .apply_chunk(chunk(3, b"def", true, None), 256 * 1024)
        .expect("chunk 2");
    let mut calls = 0usize;
    let mut collected = Vec::new();
    let report = window
        .export_stream(|bytes| {
            calls += 1;
            collected.extend_from_slice(bytes);
            true
        })
        .expect("export");
    assert_eq!(report.calls, 2);
    assert_eq!(report.bytes, 6);
    assert_eq!(calls, 2);
    assert_eq!(collected, b"abcdef");
    assert_eq!(window.loaded_text().expect("text"), "abcdef");
}

#[test]
fn detail_allows_one_in_flight_request() {
    let mut window = HistoryDetailWindow::open(descriptor(1024)).expect("open");
    assert!(window.request(0, 256 * 1024).is_some());
    assert!(
        window.request(0, 256 * 1024).is_none(),
        "single in-flight detail request"
    );
}

#[test]
fn inherited_detail_request_keeps_frozen_source() {
    let source =
        HistorySource::inherited_with_ancestor("snap-frozen", Some("ancestor-session".into()));
    let descriptor = DetailDescriptor::new(
        source.clone(),
        "s1:ancestor-message",
        1024,
        64 * 1024,
        "snap-window",
        "ticket",
    );
    let mut window = HistoryDetailWindow::open(descriptor).expect("open");
    let request = window.request(0, 64 * 1024).expect("request");
    assert_eq!(request.source, source);
    assert_eq!(request.snapshot_token, "snap-window");
}

#[test]
fn detail_failure_is_explicit_and_keeps_no_silent_empty_body() {
    let mut window = HistoryDetailWindow::open(descriptor(4)).expect("open");
    let mut wrong = chunk(0, b"body", true, None);
    wrong.entry_id = "s1:other".into();
    assert_eq!(
        window.apply_chunk(wrong, 4),
        Err(DetailError::IdentityMismatch)
    );
    assert!(window.last_error.is_some());
    assert!(window.chunk_count() == 0);
}

#[test]
fn detail_scroll_does_not_touch_window_viewport_atom() {
    crate::kit::atoms::init_atoms();
    let before = crate::kit::atoms::WINDOW_VIEWPORT.state().read().clone();
    let mut window = HistoryDetailWindow::open(descriptor(1024)).expect("open");
    let request = window.scroll_to(512).expect("scroll request");
    assert_eq!(request.offset, 512);
    assert_eq!(window.pending_offset(), Some(512));
    assert_eq!(
        crate::kit::atoms::WINDOW_VIEWPORT.state().read().clone(),
        before,
        "detail scrolling must not move the main viewport atom"
    );
}

#[test]
fn open_close_detail_uses_window_descriptor_and_keeps_slot_state() {
    crate::kit::atoms::init_atoms();
    let mut window = crate::kit::history_window::HistoryWindow::new(
        crate::kit::history_window::HistoryWindowConfig::enabled(),
    );
    let descriptor = descriptor(1024);
    let key = descriptor.key.clone();
    window.register_detail_descriptor(descriptor);
    open_history_detail(&window, &key).expect("open detail");
    assert!(crate::kit::atoms::HISTORY_DETAIL.state().read().is_some());
    close_history_detail();
    assert!(crate::kit::atoms::HISTORY_DETAIL.state().read().is_none());
}

#[test]
#[serial_test::serial]
fn session_boundary_clears_detail_and_focus() {
    crate::kit::atoms::init_atoms();
    let mut window = crate::kit::history_window::HistoryWindow::new(
        crate::kit::history_window::HistoryWindowConfig::enabled(),
    );
    let descriptor = descriptor(1024);
    let key = descriptor.key.clone();
    window.register_detail_descriptor(descriptor);
    open_history_detail(&window, &key).expect("open detail");
    crate::kit::atoms::FOCUSED_ENTRY
        .set(Some(crate::kit::atoms::FocusedEntry { slot: 3, key: None }));
    let old_active = crate::kit::atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    crate::kit::session_boundary::project_session_boundary(Some("next"));
    assert!(crate::kit::atoms::HISTORY_DETAIL.state().read().is_none());
    assert!(crate::kit::atoms::FOCUSED_ENTRY.state().read().is_none());
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = old_active;
    crate::kit::atoms::BRIDGE_RESET_COUNTER.set(old_reset);
}

#[test]
fn detail_failure_closes_and_reports_explicitly() {
    let mut window = HistoryDetailWindow::open(descriptor(4)).expect("open");
    window
        .apply_chunk(chunk(0, b"body", true, None), 256 * 1024)
        .expect("chunk");
    assert!(window.retained_bytes() > 0);
    window.fail("read ticket expired");
    assert!(
        window
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("expired"))
    );
    assert_eq!(window.retained_bytes(), 0);
    assert_eq!(window.chunk_count(), 0);
}

#[test]
#[serial_test::serial]
fn event_layer_opens_detail_from_placeholder_and_clears_on_close() {
    use crate::kit::atoms::{
        ACTIVE_PANEL, HISTORY_DETAIL, HISTORY_DETAIL_TARGETS, VIEW_MODELS, ViewModelsSnapshot,
    };
    use crate::kit::history_window::HistoryDetailTarget;
    use crate::kit::tui_render_unit::{TuiNoteLevel, TuiRenderUnit, TuiSystemNote};

    crate::kit::atoms::init_atoms();
    let descriptor = descriptor(1024);
    let hash =
        crate::kit::history_window::placeholder_hash(&descriptor.key, descriptor.total_bytes);
    *VIEW_MODELS.state().write() = ViewModelsSnapshot {
        generation: 1,
        items: vec![TuiRenderUnit::TuiSystemNote(TuiSystemNote {
            text: "超大内容".into(),
            level: TuiNoteLevel::Info,
            content_hash: hash,
        })]
        .into(),
    };
    HISTORY_DETAIL_TARGETS.set(vec![HistoryDetailTarget {
        content_hash: hash,
        descriptor: descriptor.clone(),
    }]);
    assert!(open_detail_from_slot(0));
    assert!(HISTORY_DETAIL.state().read().is_some());
    assert_eq!(
        *ACTIVE_PANEL.state().read(),
        Some(crate::app::panel_types::PanelKind::HistoryDetail)
    );
    close_history_detail();
    crate::kit::panel_registry::close_active_panel();
    assert!(HISTORY_DETAIL.state().read().is_none());
    HISTORY_DETAIL_TARGETS.set(Vec::new());
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
}

#[test]
#[serial_test::serial]
fn detail_panel_lines_show_loading_error_and_inherited_notice() {
    crate::kit::atoms::init_atoms();
    let theme = peri_theme::atoms::THEME_ATOM.state().read().clone();
    let mut window = HistoryDetailWindow::open(descriptor(4)).expect("open");
    window.fail("ticket expired");
    let lines = build_history_detail_lines(Some(&window), Some("inherited unsupported"), &theme);
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("inherited unsupported"));
    assert!(rendered.contains("ticket expired"));
    assert!(rendered.contains("s1:m1"));

    let lines = build_history_detail_lines(None, None, &theme);
    assert!(!lines.is_empty());
}

#[test]
#[serial_test::serial]
fn detail_scroll_event_path_requests_next_chunk_without_main_viewport_change() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::HISTORY_DETAIL.set(Some(
        HistoryDetailWindow::open(descriptor(1024)).expect("open"),
    ));
    let before = crate::kit::atoms::WINDOW_VIEWPORT.state().read().clone();
    scroll_detail(1);
    let detail = crate::kit::atoms::HISTORY_DETAIL
        .state()
        .read()
        .clone()
        .expect("detail");
    assert!(detail.scroll_offset > 0);
    assert!(
        detail.last_error.is_some(),
        "no ACP client in test -> explicit failure"
    );
    assert_eq!(
        crate::kit::atoms::WINDOW_VIEWPORT.state().read().clone(),
        before
    );
    close_history_detail();
}
