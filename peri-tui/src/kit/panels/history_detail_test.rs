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
fn detail_scroll_event_path_uses_one_registered_request_per_chunk() {
    crate::kit::atoms::init_atoms();
    let descriptor = DetailDescriptor::new(
        HistorySource::Own,
        "s1:m1",
        4096,
        1024,
        "snap-1",
        "ticket-1",
    );
    crate::kit::atoms::HISTORY_DETAIL
        .set(Some(HistoryDetailWindow::open(descriptor).expect("open")));

    let first = scroll_detail_request(1).expect("first scroll must request a chunk");
    assert_eq!(
        first.offset, 1024,
        "scroll_to registers the uncovered offset"
    );
    {
        let handle = crate::kit::atoms::HISTORY_DETAIL.state();
        let detail = handle.read();
        let detail = detail.as_ref().expect("detail");
        assert_eq!(
            detail.pending_offset(),
            Some(first.offset),
            "the request returned to the UI is the one pending; no second request"
        );
    }
    // 响应落地后 pending 清空，下一次滚动可以继续登记并发送。
    crate::kit::atoms::HISTORY_DETAIL
        .state()
        .write()
        .as_mut()
        .expect("detail")
        .apply_chunk(chunk(1024, &vec![b'a'; 1024], false, Some(2048)), 1024)
        .expect("apply first request");
    let second = scroll_detail_request(1).expect("second scroll must request a chunk");
    assert_eq!(second.offset, 2048);
    close_history_detail();
}

#[test]
#[serial_test::serial]
fn detail_visible_text_survives_first_chunk_eviction_and_prefix_refetch() {
    crate::kit::atoms::init_atoms();
    let total = 5 * 1024 * 1024u64;
    let step = 256 * 1024usize;
    let mut window = HistoryDetailWindow::open(descriptor(total)).expect("open");
    let block = vec![b'x'; step];

    // 先铺满 4 MiB 预算，再滚到 4 MiB 处加载新块；以视口为锚会淘汰最早块。
    for offset in (0..4 * 1024 * 1024).step_by(step) {
        window
            .apply_chunk(
                chunk(
                    offset as u64,
                    &block,
                    false,
                    Some(offset as u64 + step as u64),
                ),
                step as u32,
            )
            .expect("chunk");
    }
    assert_eq!(window.retained_bytes(), DETAIL_WINDOW_MAX_BYTES);
    let request = window.scroll_to(4 * 1024 * 1024).expect("scroll request");
    assert_eq!(request.offset, 4 * 1024 * 1024);
    window
        .apply_chunk(
            chunk(
                4 * 1024 * 1024,
                &block,
                false,
                Some(4 * 1024 * 1024 + step as u64),
            ),
            step as u32,
        )
        .expect("tail chunk");
    let visible = window.visible_text().expect("visible text");
    assert!(
        !visible.text.is_empty(),
        "evicting the first chunk must not blank the panel"
    );
    assert!(
        visible.prefix_evicted,
        "prefix gap is reported to the panel"
    );
    assert!(window.chunk_arc(0).is_none(), "oldest chunk was evicted");

    // 滚回开头重新请求首块，落地后显示从 0 开始。
    let request = window.scroll_to(0).expect("prefix request");
    assert_eq!(request.offset, 0);
    window
        .apply_chunk(chunk(0, b"prefix", false, Some(6)), step as u32)
        .expect("prefix chunk");
    let visible = window.visible_text().expect("visible text");
    assert!(visible.text.starts_with("prefix"));
    assert!(!visible.prefix_evicted);
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

#[test]
fn detail_refetches_evicted_prefix_after_complete() {
    let total = 5 * 1024 * 1024u64;
    let step = 256 * 1024usize;
    let mut window = HistoryDetailWindow::open(descriptor(total)).expect("open");
    let block = vec![b'x'; step];
    let last_offset = total - step as u64;

    let mut offset = 0u64;
    while offset < last_offset {
        window
            .apply_chunk(
                chunk(offset, &block, false, Some(offset + step as u64)),
                step as u32,
            )
            .expect("sequential chunk");
        offset += step as u64;
    }
    // 把视口移到尾部，首块才会在预算压力下被淘汰。
    let request = window.scroll_to(last_offset).expect("scroll to tail");
    assert_eq!(request.offset, last_offset);
    window
        .apply_chunk(chunk(last_offset, &block, true, None), step as u32)
        .expect("final chunk");
    assert!(window.is_complete(), "EOF reached");
    assert!(
        window.chunk_arc(0).is_none(),
        "budget must have evicted the first chunk"
    );

    // EOF 只表示顺序读到末尾，不表示前缀仍驻留：回读必须重新请求。
    let request = window.scroll_to(0).expect("evicted prefix must refetch");
    assert_eq!(request.offset, 0);
    window
        .apply_chunk(chunk(0, &block, false, Some(step as u64)), step as u32)
        .expect("refetched prefix");
    let visible = window.visible_text().expect("visible text after refetch");
    assert!(
        visible.text.starts_with('x'),
        "refetched prefix must be displayed from offset 0"
    );
    assert!(!visible.prefix_evicted);
    assert!(window.is_complete(), "EOF fact survives prefix refetch");
}

#[test]
fn detail_visible_text_truncates_at_a_char_boundary_when_prefix_is_evicted() {
    let text = "中文".as_bytes();
    let mut window = HistoryDetailWindow::open(descriptor(text.len() as u64)).expect("open");
    // 首块从第二个字节开始：起点落在「中」的多字节编码中间。
    window
        .request(1, 256 * 1024)
        .expect("request from a mid-character offset");
    window
        .apply_chunk(
            chunk(1, &text[1..], false, Some(text.len() as u64)),
            256 * 1024,
        )
        .expect("chunk");

    let visible = window.visible_text().expect("fragment is not corruption");
    assert_eq!(visible.text, "文");
    assert!(visible.prefix_evicted);
    assert_eq!(visible.skipped_fragment_bytes, 2);
}

#[test]
fn detail_visible_text_keeps_valid_prefix_when_tail_is_truncated() {
    // 64 KiB 分块的有效中文正文：块尾落在多字节字符中间。修复前整段正文显示为
    // UTF-8 错误；修复后应保留有效前缀，等下一块补齐。
    let body = "中".repeat(64 * 1024);
    let bytes = body.as_bytes();
    let first_len = 64 * 1024 - 2;
    assert_ne!(
        first_len % 3,
        0,
        "fixture must cut inside a three-byte character"
    );
    let mut window = HistoryDetailWindow::open(descriptor(bytes.len() as u64)).expect("open");
    window
        .apply_chunk(
            chunk(0, &bytes[..first_len], false, Some(first_len as u64)),
            256 * 1024,
        )
        .expect("first chunk");

    let visible = window
        .visible_text()
        .expect("mid-character tail is not corruption");
    assert_eq!(visible.text.as_bytes(), &bytes[..first_len - 2]);
    assert_eq!(visible.trailing_fragment_bytes, 2);
    assert!(!visible.text.is_empty(), "valid prefix must stay visible");

    window
        .apply_chunk(
            chunk(first_len as u64, &bytes[first_len..], true, None),
            256 * 1024,
        )
        .expect("second chunk");
    let visible = window.visible_text().expect("complete text");
    assert_eq!(visible.text, body);
    assert_eq!(visible.trailing_fragment_bytes, 0);
}

#[test]
fn detail_incomplete_tail_at_eof_is_still_corruption() {
    let bytes = b"ab\xe4\xb8";
    let mut window = HistoryDetailWindow::open(descriptor(bytes.len() as u64)).expect("open");
    window
        .apply_chunk(chunk(0, bytes, true, None), 256 * 1024)
        .expect("chunk");
    assert_eq!(window.visible_text(), Err(DetailError::Utf8));
}

#[test]
fn detail_true_corruption_still_reports_utf8() {
    let mut window = HistoryDetailWindow::open(descriptor(9)).expect("open");
    let mut bytes = b"abcde".to_vec();
    bytes.push(0xFF);
    bytes.extend_from_slice(b"xyz");
    window
        .apply_chunk(chunk(0, &bytes, true, None), 256 * 1024)
        .expect("chunk");
    assert_eq!(window.visible_text(), Err(DetailError::Utf8));
}

#[test]
#[serial_test::serial]
fn detail_export_reports_prefix_gap_instead_of_copying_empty() {
    crate::kit::atoms::init_atoms();
    let mut window = HistoryDetailWindow::open(descriptor(1024)).expect("open");
    window.request(256, 256 * 1024).expect("evicted prefix");
    window
        .apply_chunk(chunk(256, b"tail", false, Some(260)), 256 * 1024)
        .expect("chunk");
    assert_eq!(window.loaded_text(), Err(DetailError::ExportGap));

    crate::kit::atoms::HISTORY_DETAIL.set(Some(window));
    export_detail_to_clipboard();
    let detail = crate::kit::atoms::HISTORY_DETAIL
        .state()
        .read()
        .clone()
        .expect("detail");
    assert!(
        detail
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("前缀已释放")),
        "export must explain the prefix gap: {:?}",
        detail.last_error
    );
    close_history_detail();
}
