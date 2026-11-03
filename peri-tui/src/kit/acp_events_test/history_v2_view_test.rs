//! H5c：V2 视图投影 → `committed` 前缀、翻页/seek/正文占位与错误态。

use peri_acp_types::history_v2::{
    HISTORY_V2_VERSION, HistoryDirection, HistoryEntryAnchor, HistoryErrorReason, HistoryLimits,
    HistorySource,
};
use serde_json::{Value, json};
use serial_test::serial;

use super::*;
use crate::kit::atoms;
use crate::kit::history_v2::consumer;
use crate::kit::history_v2::error::HistoryV2WireFailure;
use crate::kit::history_v2::initial::{admit_load_response, admit_open_response};
use crate::kit::history_v2::paging::HistoryV2PagingStatus;
use crate::kit::tui_render_unit::TuiNoteLevel;
use peri_acp_types::history_v2::DecimalU64;

struct V2Guard;

impl Drop for V2Guard {
    fn drop(&mut self) {
        atoms::HISTORY_V2_CAPABLE.set(false);
        atoms::HISTORY_V2_STATE.set(crate::kit::history_v2::HistoryV2State::default());
        atoms::HISTORY_DETAIL.set(None);
        consumer::reset_consumer_state();
    }
}

fn setup() -> V2Guard {
    atoms::init_atoms();
    atoms::HISTORY_V2_CAPABLE.set(true);
    atoms::HISTORY_V2_LIMITS.set(HistoryLimits::default());
    atoms::HISTORY_V2_PREFIX_LEN.set(0);
    atoms::HISTORY_V2_VIEW_EPOCH.set(0);
    atoms::HISTORY_DETAIL_TARGETS.set(Vec::new());
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    V2Guard
}

fn item(id: &str, ordinal: u64, sub_items: Value) -> Value {
    json!({
        "entryId": format!("s1:{id}"),
        "source": {"kind": "own"},
        "contentVersion": "1",
        "durability": {"state": "sealed", "origin": "live", "finalItemSeq": "3"},
        "order": {"region": "own", "ordinal": ordinal.to_string()},
        "subItems": sub_items,
    })
}

fn text(id: &str, role: &str, text: &str) -> Value {
    json!({
        "kind": "text",
        "subItemId": format!("s1:{id}:0"),
        "role": role,
        "text": text,
    })
}

fn opaque(id: &str) -> Value {
    json!({
        "kind": "opaque",
        "subItemId": format!("s1:{id}:opaque"),
        "body": {
            "bodyRef": format!("body-{id}"),
            "representation": "display-text",
            "mediaType": "text/plain; charset=utf-8",
            "totalBytes": "2048",
            "chunkBytes": 1024,
        },
    })
}

fn page(before: Option<&str>, items: Value) -> Value {
    json!({
        "version": HISTORY_V2_VERSION,
        "kind": "window",
        "sessionId": "s1",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "historyRevision": "17",
        "committedTail": "19",
        "projectionVersion": 1,
        "readTicket": "ticket-1",
        "beforeCursor": before,
        "afterCursor": "after-1",
        "items": items,
        "coverage": {"kind": "interval", "from": {"kind": "head"}, "to": {"kind": "tail"}},
        "progress": {"reason": "complete", "scannedEntries": 1, "consumedEntries": 1},
    })
}

fn admitted() -> crate::kit::history_v2::HistoryV2State {
    let response = json!({
        "_meta": {
            "peri.history": {
                "version": HISTORY_V2_VERSION,
                "kind": "initial",
                "requestKey": "1",
                "executionReady": true,
                "runtimeGeneration": "runtime-9",
                "deliveryId": "delivery-51",
                "liveBoundary": {"streamId": "live-7", "afterSeq": "120"},
                "page": page(Some("before-1"), json!([
                    item("e3", 7, json!([text("e3", "assistant", "newest")])),
                    item("e2", 6, json!([opaque("e2")])),
                ])),
            }
        }
    });
    admit_load_response(
        &response,
        "s1",
        DecimalU64::new(1),
        &HistoryLimits::default(),
    )
    .expect("admitted")
}

fn dispatch_view(state: &mut BridgeState, v2: &crate::kit::history_v2::HistoryV2State) {
    atoms::HISTORY_V2_STATE.set(v2.clone());
    dispatch_and_notify(
        state,
        &crate::kit::acp_events::history_v2::view_event(v2.clone()),
    );
}

#[test]
#[serial]
fn initial_projects_one_prefix_with_opaque_placeholder_and_target() {
    let _guard = setup();
    let v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);

    assert_eq!(state.committed.len(), 2);
    assert_eq!(atoms::HISTORY_V2_PREFIX_LEN.get(), 2);
    assert!(matches!(
        &state.committed[0],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "newest"
    ));
    let placeholder = match &state.committed[1] {
        TuiRenderUnit::TuiSystemNote(note) => note,
        other => panic!("opaque entry must render as a note, got {other:?}"),
    };
    let targets = atoms::HISTORY_DETAIL_TARGETS.state().read().clone();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].content_hash, placeholder.content_hash);
    let descriptor = &targets[0].descriptor;
    assert_eq!(descriptor.total_bytes, 2048);
    assert_eq!(descriptor.chunk_size, 1024);
    assert_eq!(
        descriptor
            .v2
            .as_ref()
            .map(|binding| binding.sub_item_id.as_str()),
        Some("s1:e2:opaque")
    );
}

#[test]
#[serial]
fn before_page_prepends_without_touching_live_suffix() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);
    // 模拟 live 路径在 V2 前缀之后追加当前 turn 的内容。
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "live".into(),
        )));

    let limits = HistoryLimits::default();
    let request = v2
        .window_mut()
        .unwrap()
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .unwrap();
    let before = serde_json::from_value::<peri_acp_types::history_v2::HistoryWindowPage>(page(
        None,
        json!([item("e1", 5, json!([text("e1", "user", "older")]))]),
    ))
    .unwrap();
    assert!(matches!(
        v2.window_mut()
            .unwrap()
            .settle_page(&request, &before, &limits),
        crate::kit::history_v2::paging::HistoryV2PageSettlement::Accepted { .. }
    ));
    dispatch_view(&mut state, &v2);

    assert_eq!(atoms::HISTORY_V2_PREFIX_LEN.get(), 3);
    assert_eq!(state.committed.len(), 4);
    assert!(matches!(
        &state.committed[0],
        TuiRenderUnit::TuiUserBubble(bubble) if bubble.text == "older"
    ));
    assert!(matches!(
        &state.committed[3],
        TuiRenderUnit::TuiUserBubble(bubble) if bubble.text == "live"
    ));
}

#[test]
#[serial]
fn seek_keeps_live_tail_and_marks_it_as_live() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "live".into(),
        )));

    let limits = HistoryLimits::default();
    let anchor = HistoryEntryAnchor {
        entry_id: "s1:e1".into(),
        source: HistorySource::Own,
    };
    let request = v2
        .window_mut()
        .unwrap()
        .request_seek(DecimalU64::new(3), &anchor, 25, 25, &limits)
        .unwrap();
    let target = serde_json::from_value::<peri_acp_types::history_v2::HistoryWindowPage>(page(
        Some("before-9"),
        json!([item("e1", 3, json!([text("e1", "assistant", "target")]))]),
    ))
    .unwrap();
    assert!(matches!(
        v2.window_mut()
            .unwrap()
            .settle_seek(&request, &target, &limits),
        crate::kit::history_v2::paging::HistoryV2SeekSettlement::Accepted { .. }
    ));
    dispatch_view(&mut state, &v2);

    assert_eq!(
        state.committed.len(),
        3,
        "seek 只替换历史页，未持久化 live 尾部必须保留"
    );
    assert!(matches!(
        &state.committed[0],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "target"
    ));
    assert!(matches!(
        &state.committed[1],
        TuiRenderUnit::TuiSystemNote(note) if note.text.contains("实时内容")
    ));
    assert!(matches!(
        &state.committed[2],
        TuiRenderUnit::TuiUserBubble(bubble) if bubble.text == "live"
    ));
    assert_eq!(atoms::HISTORY_V2_PREFIX_LEN.get(), 1);
}

#[test]
#[serial]
fn full_flow_pages_seeks_reads_body_and_survives_window_expiry() {
    let _guard = setup();
    let limits = HistoryLimits::default();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();

    // initial：首屏进入 V2 前缀。
    dispatch_view(&mut state, &v2);
    assert_eq!(atoms::HISTORY_V2_PREFIX_LEN.get(), 2);

    // before 翻页：同代次只替换前缀。
    let page_request = v2
        .window_mut()
        .unwrap()
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .unwrap();
    let older = serde_json::from_value::<peri_acp_types::history_v2::HistoryWindowPage>(page(
        None,
        json!([item("e1", 5, json!([text("e1", "user", "older")]))]),
    ))
    .unwrap();
    assert!(matches!(
        v2.window_mut()
            .unwrap()
            .settle_page(&page_request, &older, &limits),
        crate::kit::history_v2::paging::HistoryV2PageSettlement::Accepted { .. }
    ));
    dispatch_view(&mut state, &v2);
    assert_eq!(state.committed.len(), 3);

    // seek：新导航代次替换历史页，未持久化 live 尾部保留并标记。
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "live".into(),
        )));
    let seek_request = v2
        .window_mut()
        .unwrap()
        .request_seek(
            DecimalU64::new(3),
            &HistoryEntryAnchor {
                entry_id: "s1:e0".into(),
                source: HistorySource::Own,
            },
            25,
            25,
            &limits,
        )
        .unwrap();
    let target = serde_json::from_value::<peri_acp_types::history_v2::HistoryWindowPage>(page(
        Some("before-9"),
        json!([
            item("e1", 3, json!([text("e1", "assistant", "target")])),
            item("e0", 2, json!([opaque("e0")])),
        ]),
    ))
    .unwrap();
    assert!(matches!(
        v2.window_mut()
            .unwrap()
            .settle_seek(&seek_request, &target, &limits),
        crate::kit::history_v2::paging::HistoryV2SeekSettlement::Accepted { .. }
    ));
    dispatch_view(&mut state, &v2);
    assert_eq!(
        state.committed.len(),
        4,
        "seek 后保留 live 尾部（target + opaque + marker + live）"
    );
    assert_eq!(atoms::HISTORY_V2_PREFIX_LEN.get(), 2);
    assert!(matches!(
        state.committed.last(),
        Some(TuiRenderUnit::TuiUserBubble(bubble)) if bubble.text == "live"
    ));

    // body：占位 note 打开详情后按块读取，不内联 JSON。
    let slot = VIEW_MODELS
        .state()
        .read()
        .items
        .iter()
        .position(|unit| matches!(unit, TuiRenderUnit::TuiSystemNote(_)))
        .expect("opaque placeholder");
    assert!(crate::kit::panels::history_detail::open_detail_from_slot(
        slot
    ));
    let chunk = serde_json::from_value::<peri_acp_types::history_v2::HistoryBodyChunk>(json!({
        "version": HISTORY_V2_VERSION,
        "kind": "body",
        "sessionId": "s1",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "bodyRef": "body-e0",
        "contentVersion": "1",
        "representation": "display-text",
        "encoding": "base64",
        "offset": "0",
        "data": "eyJvayI6dHJ1ZX0=",
        "nextOffset": null,
        "complete": true,
        "totalBytes": "11",
    }))
    .unwrap();
    {
        let handle = atoms::HISTORY_DETAIL.state();
        let mut guard = handle.write();
        let detail = guard.as_mut().expect("detail open");
        let generation = detail.attempt_generation();
        detail
            .apply_v2_chunk(&chunk, 256 * 1024, generation)
            .expect("body chunk accepted");
        assert_eq!(detail.loaded_text().expect("utf8"), "{\"ok\":true}");
    }
    atoms::HISTORY_DETAIL.set(None);

    // 窗口失效：保留已读页，状态进入重开，视图追加警告 note。
    let page_request = v2
        .window_mut()
        .unwrap()
        .request_page(DecimalU64::new(4), HistoryDirection::Before, 50, &limits)
        .unwrap();
    let failure = HistoryV2WireFailure {
        code: -32000,
        message: String::new(),
        reason: Some(HistoryErrorReason::WindowExpired),
        recovery: Some(peri_acp_types::history_v2::HistoryRecovery::Reopen),
    };
    assert!(!consumer::apply_failure(
        &mut v2,
        page_request.context.identity(),
        &failure
    ));
    dispatch_view(&mut state, &v2);
    assert!(matches!(
        v2.window().unwrap().paging.status,
        HistoryV2PagingStatus::ReopenRequired { .. }
    ));
    assert_eq!(v2.window().unwrap().items.len(), 2, "旧页仍可读");
    let note = state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiSystemNote(note) if note.level == TuiNoteLevel::Warning => Some(note),
            _ => None,
        })
        .expect("expected warning note");
    assert_eq!(note.level, TuiNoteLevel::Warning);
    assert!(
        matches!(
            state.committed.last(),
            Some(TuiRenderUnit::TuiUserBubble(bubble)) if bubble.text == "live"
        ),
        "窗口失效后未持久化 live 尾部仍保留在末尾"
    );
}

#[test]
#[serial]
fn opaque_placeholder_opens_detail_and_loads_v2_chunk() {
    let _guard = setup();
    let v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);

    let slot = VIEW_MODELS
        .state()
        .read()
        .items
        .iter()
        .position(|unit| matches!(unit, TuiRenderUnit::TuiSystemNote(_)))
        .expect("placeholder slot");
    assert!(crate::kit::panels::history_detail::open_detail_from_slot(
        slot
    ));
    let chunk = serde_json::from_value::<peri_acp_types::history_v2::HistoryBodyChunk>(json!({
        "version": HISTORY_V2_VERSION,
        "kind": "body",
        "sessionId": "s1",
        "windowId": "w1",
        "snapshotToken": "snapshot-1",
        "bodyRef": "body-e2",
        "contentVersion": "1",
        "representation": "display-text",
        "encoding": "base64",
        "offset": "0",
        "data": "eyJvayI6dHJ1ZX0=",
        "nextOffset": null,
        "complete": true,
        "totalBytes": "11",
    }))
    .unwrap();
    let handle = atoms::HISTORY_DETAIL.state();
    let mut guard = handle.write();
    let detail = guard.as_mut().expect("detail open");
    let generation = detail.attempt_generation();
    detail
        .apply_v2_chunk(&chunk, 256 * 1024, generation)
        .expect("v2 chunk accepted");
    let text = detail.loaded_text().expect("utf8");
    assert_eq!(
        text, "{\"ok\":true}",
        "不得内联超大 JSON：正文只在打开详情后按块读取"
    );
    // retained_bytes 记录块容量用于预算，至少覆盖实际读入的字节。
    assert!(detail.retained_bytes() >= text.len());
}

#[test]
#[serial]
fn window_expiry_renders_warning_note_and_keeps_retained_pages() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);

    let limits = HistoryLimits::default();
    let request = v2
        .window_mut()
        .unwrap()
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .unwrap();
    let failure = HistoryV2WireFailure {
        code: -32000,
        message: String::new(),
        reason: Some(HistoryErrorReason::WindowClosed),
        recovery: Some(peri_acp_types::history_v2::HistoryRecovery::Reopen),
    };
    assert!(!consumer::apply_failure(
        &mut v2,
        request.context.identity(),
        &failure
    ));
    dispatch_view(&mut state, &v2);

    assert!(matches!(
        v2.window().unwrap().paging.status,
        HistoryV2PagingStatus::ReopenRequired { .. }
    ));
    // 旧页仍可读；警告 note 在前缀末尾。
    assert_eq!(atoms::HISTORY_V2_PREFIX_LEN.get(), 3);
    let note = match state.committed.last() {
        Some(TuiRenderUnit::TuiSystemNote(note)) => note,
        other => panic!("expected warning note, got {other:?}"),
    };
    assert!(note.text.contains("history_window_closed") || note.text.contains("window_closed"));
    assert_eq!(note.level, TuiNoteLevel::Warning);
}

#[test]
#[serial]
fn same_epoch_page_keeps_detail_but_seek_closes_it() {
    let _guard = setup();
    let limits = HistoryLimits::default();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);

    let slot = VIEW_MODELS
        .state()
        .read()
        .items
        .iter()
        .position(|unit| matches!(unit, TuiRenderUnit::TuiSystemNote(_)))
        .expect("opaque placeholder");
    assert!(crate::kit::panels::history_detail::open_detail_from_slot(
        slot
    ));
    assert!(atoms::HISTORY_DETAIL.state().read().is_some());

    // 同代次 before 翻页只替换前缀，详情引用的窗口未变，保持打开。
    let page_request = v2
        .window_mut()
        .unwrap()
        .request_page(DecimalU64::new(2), HistoryDirection::Before, 50, &limits)
        .unwrap();
    let older = serde_json::from_value::<peri_acp_types::history_v2::HistoryWindowPage>(page(
        None,
        json!([item("e1", 5, json!([text("e1", "user", "older")]))]),
    ))
    .unwrap();
    assert!(matches!(
        v2.window_mut()
            .unwrap()
            .settle_page(&page_request, &older, &limits),
        crate::kit::history_v2::paging::HistoryV2PageSettlement::Accepted { .. }
    ));
    dispatch_view(&mut state, &v2);
    assert!(
        atoms::HISTORY_DETAIL.state().read().is_some(),
        "同代次翻页不得关闭正在阅读的详情"
    );

    // seek 推进导航代次：旧 window/snapshot/bodyRef 全部作废，详情必须收起。
    let seek_request = v2
        .window_mut()
        .unwrap()
        .request_seek(
            DecimalU64::new(3),
            &HistoryEntryAnchor {
                entry_id: "s1:e1".into(),
                source: HistorySource::Own,
            },
            25,
            25,
            &limits,
        )
        .unwrap();
    let target = serde_json::from_value::<peri_acp_types::history_v2::HistoryWindowPage>(page(
        Some("before-9"),
        json!([item("e1", 3, json!([text("e1", "assistant", "target")]))]),
    ))
    .unwrap();
    assert!(matches!(
        v2.window_mut()
            .unwrap()
            .settle_seek(&seek_request, &target, &limits),
        crate::kit::history_v2::paging::HistoryV2SeekSettlement::Accepted { .. }
    ));
    dispatch_view(&mut state, &v2);
    assert!(
        atoms::HISTORY_DETAIL.state().read().is_none(),
        "seek 推进导航代次后，旧窗口的详情必须收起"
    );
}

#[test]
#[serial]
fn v2_state_is_inactive_without_server_selection() {
    let _guard = setup();
    atoms::HISTORY_V2_CAPABLE.set(false);
    assert!(!crate::kit::history_v2::consumer::v2_path_active());
}

fn tool_call(invocation: &str, title: &str) -> Value {
    json!({
        "kind": "toolCall",
        "subItemId": format!("s1:{invocation}:call"),
        "invocationKey": invocation,
        "title": title,
        "status": "running",
        "input": {"path": "/tmp/a"},
    })
}

fn tool_result(invocation: &str, output: &str) -> Value {
    json!({
        "kind": "toolResult",
        "subItemId": format!("s1:{invocation}:result"),
        "invocationKey": invocation,
        "status": "completed",
        "output": output,
    })
}

#[test]
#[serial]
fn seek_dedupes_live_message_already_in_the_new_page() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "same message".into(),
        )));

    let limits = HistoryLimits::default();
    let request = v2
        .window_mut()
        .unwrap()
        .request_seek(
            DecimalU64::new(3),
            &HistoryEntryAnchor {
                entry_id: "s1:e1".into(),
                source: HistorySource::Own,
            },
            25,
            25,
            &limits,
        )
        .unwrap();
    let target = serde_json::from_value::<peri_acp_types::history_v2::HistoryWindowPage>(page(
        Some("before-9"),
        json!([item("e1", 3, json!([text("e1", "user", "same message")]))]),
    ))
    .unwrap();
    assert!(matches!(
        v2.window_mut()
            .unwrap()
            .settle_seek(&request, &target, &limits),
        crate::kit::history_v2::paging::HistoryV2SeekSettlement::Accepted { .. }
    ));
    dispatch_view(&mut state, &v2);

    assert_eq!(
        state.committed.len(),
        1,
        "新页已包含同一消息时不得再保留 live 副本"
    );
    assert!(matches!(
        &state.committed[0],
        TuiRenderUnit::TuiUserBubble(bubble) if bubble.text == "same message"
    ));
    assert_eq!(atoms::HISTORY_V2_PREFIX_LEN.get(), 1);
}

#[test]
#[serial]
fn reopen_keeps_streaming_content_not_in_the_new_page() {
    let _guard = setup();
    let limits = HistoryLimits::default();
    let v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);
    state
        .committed
        .push_back(crate::kit::acp_events::turn::committed_assistant_unit(
            "streaming",
            &None,
        ));

    let mut reopened = page(
        Some("before-2"),
        json!([item("e9", 9, json!([text("e9", "assistant", "persisted")]))]),
    );
    reopened["windowId"] = json!("w2");
    reopened["snapshotToken"] = json!("snapshot-2");
    reopened["readTicket"] = json!("ticket-2");
    let opened = crate::kit::history_v2::initial::admit_open_response(
        &v2,
        "s1",
        &serde_json::from_value(reopened).unwrap(),
        &limits,
        DecimalU64::new(9),
    )
    .expect("reopened");
    dispatch_view(&mut state, &opened);

    assert!(matches!(
        &state.committed[0],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "persisted"
    ));
    assert!(matches!(
        &state.committed[1],
        TuiRenderUnit::TuiSystemNote(note) if note.text.contains("实时内容")
    ));
    assert!(matches!(
        state.committed.last(),
        Some(TuiRenderUnit::TuiAssistantBubble(bubble)) if bubble.text == "streaming"
    ));
}

#[test]
#[serial]
fn detached_keeps_the_accepted_prefix_and_shows_a_gap_warning() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "accepted".into(),
        )));
    v2.window_mut().unwrap().paging.live.detached = true;
    dispatch_view(&mut state, &v2);

    assert!(matches!(
        &state.committed[0],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "newest"
    ));
    let warning = state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiSystemNote(note)
                if note.level == TuiNoteLevel::Warning && note.text.contains("实时流缺口") =>
            {
                Some(note)
            }
            _ => None,
        })
        .expect("detached warning");
    assert!(warning.text.contains("重新加载"));
    assert!(matches!(
        state.committed.last(),
        Some(TuiRenderUnit::TuiUserBubble(bubble)) if bubble.text == "accepted"
    ));
}

#[test]
#[serial]
fn tool_call_and_result_merge_across_entries() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    v2.window.as_mut().unwrap().items = serde_json::from_value(json!([
        item("e1", 1, json!([tool_call("inv-1", "Read")])),
        item("e2", 2, json!([tool_result("inv-1", "done")])),
    ]))
    .unwrap();
    dispatch_view(&mut state, &v2);

    assert_eq!(state.committed.len(), 1, "结果不得生成第二张卡片");
    let card = match &state.committed[0] {
        TuiRenderUnit::TuiToolCard(card) => card,
        other => panic!("expected tool card, got {other:?}"),
    };
    assert_eq!(card.tool_id, "inv-1");
    assert_eq!(card.tool_name, "Read");
    assert_eq!(card.output_summary, "done");
    assert!(!card.is_error);
}

#[test]
#[serial]
fn tool_result_without_call_is_a_standalone_card() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    v2.window.as_mut().unwrap().items = serde_json::from_value(json!([item(
        "e2",
        2,
        json!([tool_result("inv-9", "orphan")])
    ),]))
    .unwrap();
    dispatch_view(&mut state, &v2);

    assert_eq!(state.committed.len(), 1);
    let card = match &state.committed[0] {
        TuiRenderUnit::TuiToolCard(card) => card,
        other => panic!("expected tool card, got {other:?}"),
    };
    assert_eq!(card.tool_name, "tool");
    assert_eq!(card.output_summary, "orphan");
}

#[test]
#[serial]
fn different_invocation_keys_do_not_merge() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    v2.window.as_mut().unwrap().items = serde_json::from_value(json!([
        item("e1", 1, json!([tool_call("inv-1", "Read")])),
        item("e2", 2, json!([tool_result("inv-2", "other")])),
    ]))
    .unwrap();
    dispatch_view(&mut state, &v2);

    assert_eq!(state.committed.len(), 2);
    assert!(matches!(
        &state.committed[0],
        TuiRenderUnit::TuiToolCard(card) if card.tool_id == "inv-1"
    ));
    assert!(matches!(
        &state.committed[1],
        TuiRenderUnit::TuiToolCard(card)
            if card.tool_id == "inv-2" && card.output_summary == "other"
    ));
}

/// rewind 只作废窗口：连接级投递事实必须保留，open 接纳后新事件才能继续被接纳。
#[test]
#[serial]
fn rewind_keeps_live_boundary_and_open_restores_admission() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);
    for seq in [121, 122] {
        assert!(matches!(
            v2.window_mut()
                .unwrap()
                .accept_live_event("live-7", DecimalU64::new(seq)),
            crate::kit::history_v2::live::HistoryV2LiveAdmission::Accepted { .. }
        ));
    }
    atoms::HISTORY_V2_STATE.set(v2);
    atoms::ACTIVE_SESSION_ID.set("s1".to_owned());

    let messages_json = json!([
        {"role": "user", "id": "msg-1", "content": "回退后"},
    ])
    .to_string();
    dispatch_and_notify(&mut state, &AcpEventData::RewindCompleted { messages_json });

    let rewound = crate::kit::history_v2::snapshot();
    assert_eq!(
        rewound
            .live_boundary
            .as_ref()
            .map(|boundary| boundary.after_seq.get()),
        Some(122),
        "rewind 后已接纳高水位必须保留"
    );
    assert_eq!(rewound.session_id.as_deref(), Some("s1"));
    assert!(rewound.window.is_none(), "旧窗口必须作废");

    // open 响应不携带 liveBoundary：接纳时用保留边界重建 paging.live。
    let reopened = page(
        Some("before-2"),
        json!([
            item("m1", 1, json!([text("m1", "user", "回退后")])),
            item("m2", 2, json!([text("m2", "assistant", "继续")])),
        ]),
    );
    let mut opened = admit_open_response(
        &rewound,
        "s1",
        &serde_json::from_value(reopened).unwrap(),
        &HistoryLimits::default(),
        DecimalU64::new(20),
    )
    .expect("open admitted after rewind");
    dispatch_view(&mut state, &opened);

    assert!(
        matches!(
            opened
                .window_mut()
                .unwrap()
                .accept_live_event("live-7", DecimalU64::new(123)),
            crate::kit::history_v2::live::HistoryV2LiveAdmission::Accepted { .. }
        ),
        "open 接纳后下一条实时更新必须正常接纳，不能停留在 NotReady"
    );
    crate::kit::history_v2::live::clear_all_pending();
}

/// rewind 回填是窗口前缀：新 open 页整体替换它，不叠加在历史页之后。
#[test]
#[serial]
fn rewind_backfill_is_replaced_by_the_opened_window() {
    let _guard = setup();
    let v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    dispatch_view(&mut state, &v2);
    atoms::HISTORY_V2_STATE.set(v2);
    atoms::ACTIVE_SESSION_ID.set("s1".to_owned());

    let backfill: Vec<Value> = (0..13)
        .map(|index| {
            json!({
                "role": "user",
                "id": format!("old-{index}"),
                "content": format!("回退消息 {index}"),
            })
        })
        .collect();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::RewindCompleted {
            messages_json: Value::Array(backfill).to_string(),
        },
    );
    assert_eq!(state.committed.len(), 13);
    assert_eq!(
        atoms::HISTORY_V2_PREFIX_LEN.get(),
        13,
        "回填内容标记为窗口前缀"
    );

    let rewound = crate::kit::history_v2::snapshot();
    let reopened = page(
        Some("before-2"),
        json!([
            item("n1", 20, json!([text("n1", "user", "新页 1")])),
            item("n2", 21, json!([text("n2", "user", "新页 2")])),
        ]),
    );
    let opened = admit_open_response(
        &rewound,
        "s1",
        &serde_json::from_value(reopened).unwrap(),
        &HistoryLimits::default(),
        DecimalU64::new(20),
    )
    .expect("open admitted after rewind");
    dispatch_view(&mut state, &opened);

    assert_eq!(
        state.committed.len(),
        2,
        "新页整体替换回填内容，不叠加整批回退历史"
    );
    assert_eq!(atoms::HISTORY_V2_PREFIX_LEN.get(), 2);
    crate::kit::history_v2::live::clear_all_pending();
}

/// 历史投影携带 `entryId` 中的消息身份，同文不同 id 的 live 尾部不得被删除。
#[test]
#[serial]
fn historical_user_bubble_carries_message_id_and_keeps_same_text_live_tail() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    v2.window.as_mut().unwrap().items =
        serde_json::from_value(json!([item("m1", 1, json!([text("m1", "user", "继续")])),]))
            .unwrap();
    dispatch_view(&mut state, &v2);

    assert_eq!(state.committed.len(), 1);
    match &state.committed[0] {
        TuiRenderUnit::TuiUserBubble(bubble) => {
            assert_eq!(bubble.message_id.as_deref(), Some("m1"));
        }
        other => panic!("expected user bubble, got {other:?}"),
    }

    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        TuiUserBubble::new("继续".into()).with_message_id("m2"),
    ));
    let reopened = page(
        Some("before-2"),
        json!([item("m1", 1, json!([text("m1", "user", "继续")]))]),
    );
    let opened = admit_open_response(
        &v2,
        "s1",
        &serde_json::from_value(reopened).unwrap(),
        &HistoryLimits::default(),
        DecimalU64::new(20),
    )
    .expect("reopened");
    dispatch_view(&mut state, &opened);

    let same_text = state
        .committed
        .iter()
        .filter(
            |unit| matches!(unit, TuiRenderUnit::TuiUserBubble(bubble) if bubble.text == "继续"),
        )
        .count();
    assert_eq!(same_text, 2, "同文不同身份的消息都必须保留");
}

/// 历史工具卡身份归一为裸 toolCallId，与 live 卡片按同一身份去重。
#[test]
#[serial]
fn historical_tool_card_uses_bare_tool_call_id_and_dedupes_live_card() {
    let _guard = setup();
    let mut v2 = admitted();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    v2.window.as_mut().unwrap().items = serde_json::from_value(json!([item(
        "e1",
        1,
        json!([tool_call("s1:inv-1", "Read")])
    ),]))
    .unwrap();
    dispatch_view(&mut state, &v2);

    assert_eq!(state.committed.len(), 1);
    let (tool_name, input_summary) = match &state.committed[0] {
        TuiRenderUnit::TuiToolCard(card) => {
            assert_eq!(card.tool_id, "inv-1", "历史 invocationKey 必须归一为裸 id");
            (card.tool_name.clone(), card.input_summary.clone())
        }
        other => panic!("expected tool card, got {other:?}"),
    };

    // live 侧 toolCallId 不带会话前缀。
    state
        .committed
        .push_back(crate::kit::acp_events::tool::replay_tool_started_unit(
            "inv-1",
            &tool_name,
            &input_summary,
            &json!({"path": "/tmp/a"}),
            None,
        ));

    let reopened = page(
        Some("before-2"),
        json!([item("e1", 1, json!([tool_call("s1:inv-1", "Read")]))]),
    );
    let opened = admit_open_response(
        &v2,
        "s1",
        &serde_json::from_value(reopened).unwrap(),
        &HistoryLimits::default(),
        DecimalU64::new(20),
    )
    .expect("reopened");
    dispatch_view(&mut state, &opened);

    assert_eq!(state.committed.len(), 1, "同一 toolCallId 只保留一张卡片");
}
