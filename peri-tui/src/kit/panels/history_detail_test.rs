use std::sync::Arc;

use base64::Engine as _;
use peri_acp_types::history_v2::{
    DecimalU64 as V2U64, HISTORY_V2_VERSION, HistoryBodyChunk as V2BodyChunk, HistoryBodyKind,
    HistoryEncoding, HistoryRepresentation,
};

use super::*;
use crate::kit::history_detail_binding::{
    DetailDescriptor, HistoryDetailTarget, HistorySource, HistoryV2BodyBinding, placeholder_hash,
};

fn descriptor(total: u64) -> DetailDescriptor {
    DetailDescriptor::new(
        HistorySource::Own,
        "s1:m1",
        total,
        256 * 1024,
        "snap-1",
        "ticket-1",
    )
    .with_v2(HistoryV2BodyBinding {
        session_id: "s1".into(),
        window_id: "w1".into(),
        snapshot_token: "snap-1".into(),
        read_ticket: "ticket-1".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
        entry_id: "s1:m1".into(),
        sub_item_id: "s1:m1:0".into(),
        body_ref: "body-1".into(),
        representation: HistoryRepresentation::DisplayText,
        media_type: "text/plain; charset=utf-8".into(),
    })
}

fn v2_chunk(
    offset: u64,
    bytes: &[u8],
    complete: bool,
    next: Option<u64>,
    total: u64,
) -> V2BodyChunk {
    v2_chunk_with(
        offset,
        bytes,
        complete,
        next,
        total,
        HistoryRepresentation::DisplayText,
        "cv-1",
    )
}

fn v2_chunk_with(
    offset: u64,
    bytes: &[u8],
    complete: bool,
    next: Option<u64>,
    total: u64,
    representation: HistoryRepresentation,
    content_version: &str,
) -> V2BodyChunk {
    V2BodyChunk {
        version: HISTORY_V2_VERSION,
        kind: HistoryBodyKind::Body,
        session_id: "s1".into(),
        window_id: "w1".into(),
        snapshot_token: "snap-1".into(),
        body_ref: "body-1".into(),
        content_version: content_version.into(),
        representation,
        encoding: HistoryEncoding::Base64,
        offset: V2U64::new(offset),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        next_offset: next.map(V2U64::new),
        complete,
        total_bytes: V2U64::new(total),
    }
}

/// 用 descriptor 的 canonical 总长构造一块正文并应用。
fn apply(
    window: &mut HistoryDetailWindow,
    offset: u64,
    bytes: &[u8],
    complete: bool,
    next: Option<u64>,
    max_bytes: u32,
) -> Result<(), DetailError> {
    let chunk = v2_chunk(offset, bytes, complete, next, window.descriptor.total_bytes);
    let generation = window.attempt_generation();
    window.apply_v2_chunk(&chunk, max_bytes, generation)
}

#[test]
fn detail_chunks_cross_utf8_boundary_without_garble() {
    let mut window = HistoryDetailWindow::open(descriptor(6)).expect("open");
    let text = "中文".as_bytes();
    apply(&mut window, 0, &text[..1], false, Some(1), 256 * 1024).expect("first byte");
    apply(&mut window, 1, &text[1..], true, None, 256 * 1024).expect("rest");
    assert_eq!(window.loaded_text().expect("utf8"), "中文");
    assert!(window.is_complete());
}

#[test]
fn detail_close_releases_chunk_allocations() {
    let mut window = HistoryDetailWindow::open(descriptor(4)).expect("open");
    apply(&mut window, 0, b"body", true, None, 256 * 1024).expect("chunk");
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
        apply(
            &mut window,
            offset,
            &block[..len],
            complete,
            (!complete).then_some(offset + len as u64),
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
    apply(&mut window, 0, b"abc", false, Some(3), 256 * 1024).expect("chunk 1");
    apply(&mut window, 3, b"def", true, None, 256 * 1024).expect("chunk 2");
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
fn detail_failure_is_explicit_and_keeps_no_silent_empty_body() {
    let mut window = HistoryDetailWindow::open(descriptor(4)).expect("open");
    let mut wrong = v2_chunk(0, b"body", true, None, 4);
    wrong.body_ref = "other-body".into();
    let generation = window.attempt_generation();
    assert_eq!(
        window.apply_v2_chunk(&wrong, 4, generation),
        Err(DetailError::IdentityMismatch)
    );
    assert!(window.last_error.is_some());
    assert!(window.chunk_count() == 0);
}

#[test]
fn detail_scroll_does_not_touch_render_prepend_epoch() {
    crate::kit::atoms::init_atoms();
    let before = crate::kit::atoms::HISTORY_PREPEND_EPOCH.get();
    let mut window = HistoryDetailWindow::open(descriptor(1024)).expect("open");
    let request = window.scroll_to(512).expect("scroll request");
    assert_eq!(request.offset, 512);
    assert_eq!(window.pending_offset(), Some(512));
    assert_eq!(
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.get(),
        before,
        "detail scrolling must not move the main viewport"
    );
}

#[test]
#[serial_test::serial]
fn open_close_detail_uses_descriptor_and_keeps_slot_state() {
    crate::kit::atoms::init_atoms();
    let descriptor = descriptor(1024);
    crate::kit::atoms::HISTORY_DETAIL
        .set(Some(HistoryDetailWindow::open(descriptor).expect("open")));
    assert!(crate::kit::atoms::HISTORY_DETAIL.state().read().is_some());
    close_history_detail();
    assert!(crate::kit::atoms::HISTORY_DETAIL.state().read().is_none());
}

#[test]
#[serial_test::serial]
fn session_boundary_clears_detail_and_focus() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::HISTORY_DETAIL.set(Some(
        HistoryDetailWindow::open(descriptor(1024)).expect("open"),
    ));
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
    apply(&mut window, 0, b"body", true, None, 256 * 1024).expect("chunk");
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
    use crate::kit::tui_render_unit::{TuiNoteLevel, TuiRenderUnit, TuiSystemNote};

    crate::kit::atoms::init_atoms();
    let descriptor = descriptor(1024);
    let hash = placeholder_hash(&descriptor.key, descriptor.total_bytes);
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
fn detail_without_v2_binding_fails_explicitly() {
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
    request_detail_chunk(0);
    let detail = crate::kit::atoms::HISTORY_DETAIL
        .state()
        .read()
        .clone()
        .expect("detail");
    assert!(
        detail
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("History V2")),
        "missing binding must fail explicitly: {:?}",
        detail.last_error
    );
    close_history_detail();
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
    )
    .with_v2(HistoryV2BodyBinding {
        session_id: "s1".into(),
        window_id: "w1".into(),
        snapshot_token: "snap-1".into(),
        read_ticket: "ticket-1".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
        entry_id: "s1:m1".into(),
        sub_item_id: "s1:m1:0".into(),
        body_ref: "body-1".into(),
        representation: HistoryRepresentation::DisplayText,
        media_type: "text/plain; charset=utf-8".into(),
    });
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
    {
        let handle = crate::kit::atoms::HISTORY_DETAIL.state();
        let mut guard = handle.write();
        let detail = guard.as_mut().expect("detail");
        apply(detail, 1024, &vec![b'a'; 1024], false, Some(2048), 1024)
            .expect("apply first request");
    }
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
        apply(
            &mut window,
            offset as u64,
            &block,
            false,
            Some(offset as u64 + step as u64),
            step as u32,
        )
        .expect("chunk");
    }
    assert!(
        window.retained_bytes() <= DETAIL_WINDOW_MAX_BYTES,
        "budget must stay bounded"
    );
    assert!(
        window.retained_bytes() > DETAIL_WINDOW_MAX_BYTES - step,
        "budget should retain the newest chunks"
    );
    let request = window.scroll_to(4 * 1024 * 1024).expect("scroll request");
    assert_eq!(request.offset, 4 * 1024 * 1024);
    apply(
        &mut window,
        4 * 1024 * 1024,
        &block,
        false,
        Some(4 * 1024 * 1024 + step as u64),
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
    apply(&mut window, 0, b"prefix", false, Some(6), step as u32).expect("prefix chunk");
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
fn detail_scroll_event_path_fails_explicitly_without_client() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::HISTORY_DETAIL.set(Some(
        HistoryDetailWindow::open(descriptor(1024)).expect("open"),
    ));
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
        apply(
            &mut window,
            offset,
            &block,
            false,
            Some(offset + step as u64),
            step as u32,
        )
        .expect("sequential chunk");
        offset += step as u64;
    }
    // 把视口移到尾部，首块才会在预算压力下被淘汰。
    let request = window.scroll_to(last_offset).expect("scroll to tail");
    assert_eq!(request.offset, last_offset);
    apply(&mut window, last_offset, &block, true, None, step as u32).expect("final chunk");
    assert!(window.is_complete(), "EOF reached");
    assert!(
        window.chunk_arc(0).is_none(),
        "budget must have evicted the first chunk"
    );

    // EOF 只表示顺序读到末尾，不表示前缀仍驻留：回读必须重新请求。
    let request = window.scroll_to(0).expect("evicted prefix must refetch");
    assert_eq!(request.offset, 0);
    apply(
        &mut window,
        0,
        &block,
        false,
        Some(step as u64),
        step as u32,
    )
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
    apply(
        &mut window,
        1,
        &text[1..],
        false,
        Some(text.len() as u64),
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
    apply(
        &mut window,
        0,
        &bytes[..first_len],
        false,
        Some(first_len as u64),
        256 * 1024,
    )
    .expect("first chunk");

    let visible = window
        .visible_text()
        .expect("mid-character tail is not corruption");
    assert_eq!(visible.text.as_bytes(), &bytes[..first_len - 2]);
    assert_eq!(visible.trailing_fragment_bytes, 2);
    assert!(!visible.text.is_empty(), "valid prefix must stay visible");

    apply(
        &mut window,
        first_len as u64,
        &bytes[first_len..],
        true,
        None,
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
    apply(&mut window, 0, bytes, true, None, 256 * 1024).expect("chunk");
    assert_eq!(window.visible_text(), Err(DetailError::Utf8));
}

#[test]
fn detail_true_corruption_still_reports_utf8() {
    let mut window = HistoryDetailWindow::open(descriptor(9)).expect("open");
    let mut bytes = b"abcde".to_vec();
    bytes.push(0xFF);
    bytes.extend_from_slice(b"xyz");
    apply(&mut window, 0, &bytes, true, None, 256 * 1024).expect("chunk");
    assert_eq!(window.visible_text(), Err(DetailError::Utf8));
}

#[test]
#[serial_test::serial]
fn detail_export_reports_prefix_gap_instead_of_copying_empty() {
    crate::kit::atoms::init_atoms();
    let mut window = HistoryDetailWindow::open(descriptor(1024)).expect("open");
    window.request(256, 256 * 1024).expect("evicted prefix");
    apply(&mut window, 256, b"tail", false, Some(260), 256 * 1024).expect("chunk");
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

fn apply_v2(
    window: &mut HistoryDetailWindow,
    chunk: &V2BodyChunk,
    max_bytes: u32,
) -> Result<(), DetailError> {
    let generation = window.attempt_generation();
    window.apply_v2_chunk(chunk, max_bytes, generation)
}

#[test]
fn v2_detail_renders_text_and_adopts_authoritative_total() {
    // 页面描述符给的是 canonical 预估（5）；body 首块给出真实展示长度（11）。
    let mut window = HistoryDetailWindow::open(descriptor(5)).expect("open");
    apply_v2(
        &mut window,
        &v2_chunk(0, b"hello ", false, Some(6), 11),
        256 * 1024,
    )
    .expect("first chunk");
    assert_eq!(window.descriptor.total_bytes, 11, "首块修正滚动范围");
    assert_eq!(window.loaded_text().expect("utf8"), "hello ");

    apply_v2(
        &mut window,
        &v2_chunk(6, b"world", true, None, 11),
        256 * 1024,
    )
    .expect("second chunk");
    assert_eq!(window.loaded_text().expect("utf8"), "hello world");
    assert!(window.is_complete());
    assert_eq!(window.scroll_to(10), None, "修正后的范围覆盖末尾");
}

#[test]
fn v2_detail_rejects_later_total_mismatch() {
    let mut window = HistoryDetailWindow::open(descriptor(2048)).expect("open");
    apply_v2(
        &mut window,
        &v2_chunk(0, b"hello", false, Some(5), 11),
        256 * 1024,
    )
    .expect("first chunk");
    let error = apply_v2(
        &mut window,
        &v2_chunk(5, b"world", true, None, 12),
        256 * 1024,
    )
    .expect_err("后续块 total 不一致");
    assert_eq!(error, DetailError::InvalidChunk);
    assert_eq!(window.descriptor.total_bytes, 11);
    assert_eq!(window.loaded_text().expect("utf8"), "hello");
}

#[test]
fn v2_detail_rejects_representation_mismatch() {
    let mut window = HistoryDetailWindow::open(descriptor(5)).expect("open");
    let error = apply_v2(
        &mut window,
        &v2_chunk_with(
            0,
            b"hello",
            true,
            None,
            5,
            HistoryRepresentation::DisplayBinary,
            "cv-1",
        ),
        256 * 1024,
    )
    .expect_err("正文表示必须与描述符一致");
    assert_eq!(error, DetailError::RepresentationMismatch);
    assert_eq!(window.chunk_count(), 0);
}

#[test]
fn v2_detail_rejects_content_version_change_between_chunks() {
    let mut window = HistoryDetailWindow::open(descriptor(10)).expect("open");
    apply_v2(
        &mut window,
        &v2_chunk_with(
            0,
            b"hello",
            false,
            Some(5),
            10,
            HistoryRepresentation::DisplayText,
            "cv-first",
        ),
        256 * 1024,
    )
    .expect("first chunk adopts contentVersion");
    let error = apply_v2(
        &mut window,
        &v2_chunk_with(
            5,
            b"world",
            true,
            None,
            10,
            HistoryRepresentation::DisplayText,
            "cv-second",
        ),
        256 * 1024,
    )
    .expect_err("后续块 contentVersion 必须一致");
    assert_eq!(error, DetailError::ContentVersionMismatch);
    assert_eq!(window.loaded_text().expect("first chunk retained"), "hello");
}

#[test]
fn v2_detail_rejects_chunk_from_a_stale_attempt_after_failure() {
    let mut window = HistoryDetailWindow::open(descriptor(5)).expect("open");
    let stale_generation = window.attempt_generation();
    window.fail("timeout");
    let chunk = v2_chunk(0, b"hello", true, None, 5);
    assert_eq!(
        window.apply_v2_chunk(&chunk, 256 * 1024, stale_generation),
        Err(DetailError::StaleAttempt)
    );
    assert_eq!(window.chunk_count(), 0);
}
