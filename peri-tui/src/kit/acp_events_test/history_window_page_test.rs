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

/// 工具卡没有 live `messageId`：open 页按 `tool_id` 给出持久条目后，未归属卡片
/// 补登记为可淘汰段，`unattributed_live_bytes` 同时回落。
#[test]
#[serial]
fn open_page_binds_unattributed_tool_card_to_page_entry() {
    let (mut state, _guard) = state_with_window();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "anchor body".into(),
            message_id: Some("m-anchor".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    crate::kit::acp_events::live_tool_links::note_turn_boundary("s1");
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-page".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let unattributed_before = state.window.budget().unattributed_live_bytes;
    assert!(
        unattributed_before > 0,
        "tool-only card starts unattributed"
    );
    let key = crate::kit::history_window::PersistentEntryKey::own("s1:m-page");
    assert!(!state.window.index().contains_entry(&key));

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
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
    let response = response(
        identity,
        vec![WindowPageItem {
            source: crate::kit::history_window::HistorySource::Own,
            entry_id: "s1:m-page".into(),
            position: Some(10),
            oversize: false,
            sub_items: vec![WindowSubItem {
                position: 0,
                update: serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tool-page",
                    "title": "Read",
                    "rawInput": { "file_path": "/tmp/x" },
                }),
                system_reminder: false,
            }],
            descriptor: None,
            descriptor_only: false,
        }],
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );

    let locator = state
        .window
        .index()
        .locate_entry(&key)
        .copied()
        .expect("page-bound tool card becomes resident");
    let segment = state.window.segment(locator.segment).expect("segment");
    assert_eq!(segment.entries[0].position, Some(10));
    assert_eq!(locator.unit_count, 1);
    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "bound tool card must leave the unattributed account"
    );
    assert!(state.window.displayed_range(&key).is_some());
    assert!(
        state.committed.iter().any(
            |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-page")
        ),
        "adopted page must not duplicate or drop the live card"
    );
}

/// 旧服务端常见顺序：纯工具轮先归档（无 `originMessageId`，保持未归属），文本轮
/// 在工具轮之后。open 页按 `tool_id` 回填这张「已存在但未记账」的卡片时，不得
/// 用会右移既有区间的 `mark_display_block`，段的布局位置也必须按 `committed`
/// 顺序插到文本段之前；否则后续释放会按错误区间删正文（预算已归还但正文残留）。
#[test]
#[serial]
fn open_page_backfill_keeps_trailing_text_slot_and_layout_order() {
    use crate::kit::history_window::{
        CommitOutcome, HistoryCommitInfo, ReleasePlan, ReleaseReason, SegmentOrigin,
    };
    use peri_acp_types::history_window::HistoryCommittedTail;
    use peri_acp_types::messages::MessageId;
    use peri_acp_types::thread::ThreadId;

    let (mut state, _guard) = state_with_window();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-late".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    crate::kit::acp_events::live_tool_links::note_turn_boundary("s1");
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "late text".into(),
            message_id: Some("m-late".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    let tool_key = crate::kit::history_window::PersistentEntryKey::own("s1:m-page");
    let text_key = crate::kit::history_window::PersistentEntryKey::own("s1:m-late");
    let page_key = crate::kit::history_window::PersistentEntryKey::own("s1:m-older");
    assert!(!state.window.index().contains_entry(&tool_key));
    assert_eq!(state.window.displayed_range(&text_key), Some((1, 1)));
    assert!(state.window.budget().unattributed_live_bytes > 0);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let context = WindowRequestContext {
        request_id: "req-backfill".into(),
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "", 0, ""),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation: state.generation,
        gap: None,
    };
    let response = response(
        identity,
        vec![
            text_item("m-older", 5, "older body"),
            WindowPageItem {
                source: crate::kit::history_window::HistorySource::Own,
                entry_id: "s1:m-page".into(),
                position: Some(10),
                oversize: false,
                sub_items: vec![WindowSubItem {
                    position: 0,
                    update: serde_json::json!({
                        "sessionUpdate": "tool_call",
                        "toolCallId": "tool-late",
                        "title": "Read",
                        "rawInput": { "file_path": "/tmp/x" },
                    }),
                    system_reminder: false,
                }],
                descriptor: None,
                descriptor_only: false,
            },
        ],
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );

    // committed 最终顺序：page(older) → tool → text。
    let committed_order: Vec<&str> = state
        .committed
        .iter()
        .map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => bubble.text.as_str(),
            TuiRenderUnit::TuiToolCard(card) => card.tool_id.as_str(),
            TuiRenderUnit::TuiSystemNote(note) => note.text.as_str(),
            _ => "<other>",
        })
        .collect();
    assert_eq!(
        committed_order,
        vec!["older body", "tool-late", "late text"]
    );

    // 回填只补账目：page 插入把 text 从 1 移到 2，工具回填不得再移位。
    assert_eq!(state.window.displayed_range(&page_key), Some((0, 1)));
    assert_eq!(state.window.displayed_range(&tool_key), Some((1, 1)));
    assert_eq!(
        state.window.displayed_range(&text_key),
        Some((2, 1)),
        "回填已存在的槽位不得右移后续展示区间"
    );
    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "归属后未归属账目必须回落"
    );

    // 布局/索引顺序与 committed 一致。
    let layout_entries: Vec<_> = state
        .window
        .layout()
        .iter()
        .filter_map(|region| match region {
            crate::kit::history_window::RegionKey::Resident(key) => state
                .window
                .segment(*key)
                .map(|segment| segment.entry_keys()),
            crate::kit::history_window::RegionKey::Gap(_) => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        layout_entries,
        vec![page_key.clone(), tool_key.clone(), text_key.clone()]
    );
    let tool_locator = *state
        .window
        .index()
        .locate_entry(&tool_key)
        .expect("tool entry");
    let text_locator = *state
        .window
        .index()
        .locate_entry(&text_key)
        .expect("text entry");
    assert_eq!((tool_locator.slot_start, tool_locator.unit_count), (1, 1));
    assert_eq!((text_locator.slot_start, text_locator.unit_count), (2, 1));
    assert_eq!(
        state.window.segment(tool_locator.segment).unwrap().origin,
        SegmentOrigin::LiveSeal
    );
    let tool_segment = state.window.segment(tool_locator.segment).unwrap().clone();
    let text_segment = state.window.segment(text_locator.segment).unwrap().clone();

    // 封印工具与文本段（工具 position 来自页，文本 position 提升到提交水位）。
    assert!(state.window.promote_live_seal_position(&text_key, 12));
    let info = HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-2".into(),
        structure_version: 8,
        checkpoint_id: "cp-backfill".into(),
        summary_entry_id: None,
        committed_tail: HistoryCommittedTail::new(
            &ThreadId::from("s1".to_string()),
            MessageId::new(),
            12,
        )
        .encode(),
    };
    assert!(matches!(
        state.window.apply_commit(&info),
        CommitOutcome::Applied { .. }
    ));

    let resident_before = state.window.budget().resident_body_bytes;
    let tool_plan = ReleasePlan {
        segment: tool_locator.segment,
        source: tool_segment.source.clone(),
        entries: tool_segment.entry_keys(),
        unit_count: tool_segment.unit_count,
        body_bytes: tool_segment.body_bytes,
        reserved_bytes: tool_segment.reserved_bytes,
        slot_range: state.window.index().slot_range(tool_locator.segment),
        before: None,
        after: None,
        before_cursor: None,
        after_cursor: None,
    };
    let outcome = crate::kit::history_window::release_window_plans(
        &mut state,
        vec![tool_plan],
        ReleaseReason::BudgetPressure,
    );
    assert_eq!(outcome.released_units, 1, "只删除工具卡所在槽位");
    assert_eq!(outcome.released_entries, 1);
    assert!(!state.committed.iter().any(
        |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-late")
    ));
    assert!(matches!(
        &state.committed[1],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "late text"
    ));
    assert_eq!(
        state.window.displayed_range(&text_key),
        Some((1, 1)),
        "工具释放后相邻文本槽位正确左移"
    );
    assert_eq!(state.window.displayed_range(&page_key), Some((0, 1)));
    assert!(!state.window.index().contains_entry(&tool_key));
    assert_eq!(
        state.window.budget().resident_body_bytes,
        resident_before.saturating_sub(tool_segment.body_bytes),
        "工具段正文字节必须从驻留账目移除"
    );

    // 文本段随后释放：删除剩余正文中的正确槽位，预算继续回落。
    let text_plan = ReleasePlan {
        segment: text_locator.segment,
        source: text_segment.source.clone(),
        entries: text_segment.entry_keys(),
        unit_count: text_segment.unit_count,
        body_bytes: text_segment.body_bytes,
        reserved_bytes: text_segment.reserved_bytes,
        slot_range: state.window.index().slot_range(text_locator.segment),
        before: None,
        after: None,
        before_cursor: None,
        after_cursor: None,
    };
    let resident_before_text = state.window.budget().resident_body_bytes;
    let outcome = crate::kit::history_window::release_window_plans(
        &mut state,
        vec![text_plan],
        ReleaseReason::BudgetPressure,
    );
    assert_eq!(outcome.released_units, 1);
    assert_eq!(state.committed.len(), 1);
    assert!(matches!(
        &state.committed[0],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "older body"
    ));
    assert_eq!(state.window.displayed_range(&page_key), Some((0, 1)));
    assert_eq!(
        state.window.budget().resident_body_bytes,
        resident_before_text.saturating_sub(text_segment.body_bytes)
    );
}

/// 16 MiB 全部被不可淘汰的 unattributed 正文占用时先拒绝；同一批字节变成
/// 可淘汰的 LiveSeal 工具段后，预算压力能释放并接纳历史页。
#[test]
#[serial]
fn page_admission_rejects_when_only_unattributed_bytes_then_accepts_after_attribution() {
    use crate::kit::history_window::{
        HistoryEntryView, HistorySource, PersistentEntryKey, SegmentDraft, SegmentOrigin,
        SegmentPlacement,
    };

    let (mut state, _guard) = state_with_window();
    let limit = state.window.budget().limits.body_bytes;
    state.window.note_unattributed_live_bytes(limit);
    let rejected_identity = state.window.identity().cloned().expect("identity");
    let rejected_generation = state.window.generation();
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context("req-rejected", rejected_generation)),
            response: Box::new(response(
                rejected_identity,
                vec![text_item("m-first", 10, "page body")],
            )),
        },
    );
    assert!(
        state.window.stats().admission_blocked > 0,
        "unattributed bytes cannot be freed for page admission"
    );
    assert_eq!(state.window.segment_count(), 0);

    // 归属成功：同一批字节转入可淘汰段（同时移出 unattributed）。
    state.window.release_unattributed_live_bytes(limit);
    state
        .window
        .register_segment(
            SegmentDraft::new(
                HistorySource::Own,
                vec![HistoryEntryView::new(
                    PersistentEntryKey::own("s1:tool-big"),
                    Some(5),
                    1,
                    limit,
                    limit,
                    Vec::new(),
                )],
                limit,
                limit,
            )
            .durable(true)
            .origin(SegmentOrigin::LiveSeal),
            SegmentPlacement::Tail,
        )
        .expect("live tool segment");
    let identity = state.window.identity().cloned().expect("identity");
    let accepted_generation = state.window.generation();
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context("req-accepted", accepted_generation)),
            response: Box::new(response(
                identity,
                vec![text_item("m-second", 20, "page body")],
            )),
        },
    );
    assert_eq!(state.window.budget().unattributed_live_bytes, 0);
    assert!(
        state
            .window
            .index()
            .contains_entry(&PersistentEntryKey::own("s1:m-second")),
        "page is admitted after the attributed tool segment frees capacity"
    );
    assert!(
        !state
            .window
            .index()
            .contains_entry(&PersistentEntryKey::own("s1:tool-big")),
        "attributed tool segment is evicted under pressure"
    );
    assert!(
        state
            .committed
            .iter()
            .any(|unit| matches!(unit, TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "page body")),
        "admitted page units enter the display stream"
    );
}

/// 真实 bridge 事件驱动的完整链路：未归属工具卡 → open 页回填归属 → commit
/// 封印 → 下一次页接纳在预算压力下驱逐该工具段并释放正文。
#[test]
#[serial]
fn bridge_event_attribution_then_budget_pressure_admits_page() {
    use crate::kit::history_window::{
        CommitOutcome, HistoryCommitInfo, HistoryWindowConfig, PersistentEntryKey, WindowLimits,
    };
    use peri_acp_types::history_window::HistoryCommittedTail;
    use peri_acp_types::messages::MessageId;
    use peri_acp_types::thread::ThreadId;

    let identity = WindowIdentity::new("s1", "gen-1", 7, "snap-1");
    let _guard = setup_atoms(&identity);
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".into();
    state.window = HistoryWindow::new(HistoryWindowConfig {
        history_window_v1: true,
        limits: WindowLimits {
            body_bytes: 6 * 1024,
            ..Default::default()
        },
    });
    state.window.begin_window(identity);
    state.phase = SessionPhase::PromptRunning;

    // 先有一张已展示的文本段，open 页才会走 adopt 路径（否则初始 open 会清空
    // 旧 committed，工具卡随旧正文一起被页替代）。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "anchor".into(),
            message_id: Some("m-anchor".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    crate::kit::acp_events::live_tool_links::note_turn_boundary("s1");
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-big".into(),
            tool_name: "Read".into(),
            input_summary: "x".repeat(8 * 1024),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(
        state.window.budget().unattributed_live_bytes > 6 * 1024,
        "无归属工具卡按不可淘汰正文记账"
    );

    // 1) open 页按 tool_id 回填：adopt-only，不插入正文，只把卡片转为可淘汰段。
    let open_identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let open_context = WindowRequestContext {
        request_id: "req-open-pressure".into(),
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "", 0, ""),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation: state.generation,
        gap: None,
    };
    let open_page = response(
        open_identity,
        vec![
            text_item("m-anchor", 5, "anchor"),
            WindowPageItem {
                source: crate::kit::history_window::HistorySource::Own,
                entry_id: "s1:m-paged".into(),
                position: Some(10),
                oversize: false,
                sub_items: vec![WindowSubItem {
                    position: 0,
                    update: serde_json::json!({
                        "sessionUpdate": "tool_call",
                        "toolCallId": "tool-big",
                        "title": "Read",
                        "rawInput": {},
                    }),
                    system_reminder: false,
                }],
                descriptor: None,
                descriptor_only: false,
            },
        ],
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(open_context),
            response: Box::new(open_page),
        },
    );
    let tool_key = PersistentEntryKey::own("s1:m-paged");
    assert!(state.window.index().locate_entry(&tool_key).is_some());
    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "after open"
    );
    assert!(
        state.window.budget().pinned_body_bytes > 0,
        "未封印的 LiveSeal 段仍是不可淘汰正文"
    );
    assert!(
        state.window.budget().resident_body_bytes > 6 * 1024,
        "归属后字节转入可淘汰账目，等待预算压力回收"
    );

    // 2) commit 封印：工具段变成可驱逐（不再是 Unpersisted）。
    let info = HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-2".into(),
        structure_version: 8,
        checkpoint_id: "cp-pressure".into(),
        summary_entry_id: None,
        committed_tail: HistoryCommittedTail::new(
            &ThreadId::from("s1".to_string()),
            MessageId::new(),
            10,
        )
        .encode(),
    };
    assert!(matches!(
        state.window.apply_commit(&info),
        CommitOutcome::Applied { .. }
    ));
    assert_eq!(
        state.window.budget().pinned_body_bytes,
        0,
        "commit 解除 Unpersisted 后段可被预算压力驱逐"
    );

    // 3) 下一次页接纳触发预算压力：先驱逐已封印工具段，再接纳新页。
    let page_identity = state.window.identity().cloned().expect("identity");
    let context = WindowRequestContext {
        request_id: "req-pressure".into(),
        session_id: "s1".into(),
        identity: page_identity.clone(),
        window_generation: state.window.generation(),
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: false,
        display_generation: state.generation,
        gap: None,
    };
    let page = response(page_identity, vec![text_item("m-new", 20, "new page body")]);
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );

    assert!(
        !state.window.index().contains_entry(&tool_key),
        "预算压力必须驱逐已归属的工具段"
    );
    assert!(!state.committed.iter().any(
        |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-big")
    ));
    assert_eq!(state.committed.len(), 1);
    assert!(matches!(
        &state.committed[0],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "new page body"
    ));
    assert!(
        state
            .window
            .index()
            .contains_entry(&PersistentEntryKey::own("s1:m-new"))
    );
    assert!(
        state.window.budget().resident_body_bytes < 6 * 1024,
        "驱逐后驻留正文回到预算内"
    );
    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "after pressure"
    );
}

/// open 页再次给出已归属工具卡的条目身份时，reconcile 不得把同一单元二次并入段。
#[test]
#[serial]
fn open_page_does_not_extend_already_attributed_tool_card() {
    let (mut state, _guard) = state_with_window();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "attributed body".into(),
            message_id: Some("m-attr".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-attr".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = crate::kit::history_window::PersistentEntryKey::own("s1:m-attr");
    let locator = *state.window.index().locate_entry(&key).expect("live entry");
    let before_units = locator.unit_count;
    let before_body = state.window.segment(locator.segment).unwrap().body_bytes;
    assert_eq!(before_units, 2);

    let identity = WindowIdentity::new("s1", "gen-3", 9, "snap-3");
    let context = WindowRequestContext {
        request_id: "req-open-attr".into(),
        session_id: "s1".into(),
        identity: WindowIdentity::new("s1", "", 0, ""),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation: state.generation,
        gap: None,
    };
    let response = response(
        identity,
        vec![WindowPageItem {
            source: crate::kit::history_window::HistorySource::Own,
            entry_id: "s1:m-attr".into(),
            position: Some(30),
            oversize: false,
            sub_items: vec![
                WindowSubItem {
                    position: 0,
                    update: serde_json::json!({
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": "attributed body" },
                        "messageId": "m-attr",
                    }),
                    system_reminder: false,
                },
                WindowSubItem {
                    position: 1,
                    update: serde_json::json!({
                        "sessionUpdate": "tool_call",
                        "toolCallId": "tool-attr",
                        "title": "Read",
                        "rawInput": { "file_path": "/tmp/x" },
                    }),
                    system_reminder: false,
                },
            ],
            descriptor: None,
            descriptor_only: false,
        }],
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );

    let locator = *state.window.index().locate_entry(&key).expect("live entry");
    assert_eq!(locator.unit_count, before_units, "no duplicate unit merge");
    assert_eq!(
        state.window.segment(locator.segment).unwrap().body_bytes,
        before_body,
        "no duplicate byte accounting"
    );
    assert_eq!(state.window.segment_count(), 1);
    assert_eq!(
        state
            .committed
            .iter()
            .filter(|unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-attr"))
            .count(),
        1,
        "adopted page must not duplicate the live card"
    );
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

/// rewind 替换整段 `committed`：live 工具归属必须一起丢弃，避免长进程累积。
#[test]
#[serial]
fn rewind_forgets_live_tool_links() {
    let (mut state, _guard) = state_with_window();
    crate::kit::acp_events::live_tool_links::note_root_tool_started(
        "s1",
        "tool-rewind",
        Some("m-rewind"),
    );
    assert!(crate::kit::acp_events::live_tool_links::owner_for_tool("s1", "tool-rewind").is_some());

    dispatch_for_bridge(
        &mut state,
        &AcpEventData::RewindCompleted {
            messages_json: "[]".into(),
        },
    );
    assert!(
        crate::kit::acp_events::live_tool_links::owner_for_tool("s1", "tool-rewind").is_none(),
        "rewind 后归属映射不得继续存在"
    );
}

/// open 身份切换按展示账本摘除旧页工具卡：对应 live 归属同步清理。
#[test]
#[serial]
fn open_removing_displayed_tool_card_forgets_live_link() {
    let (mut state, _guard) = state_with_window();
    crate::kit::acp_events::live_tool_links::note_root_tool_started(
        "s1",
        "tool-page-removed",
        Some("m-origin"),
    );
    load_page(
        &mut state,
        "r-tool-removed",
        WindowPagePlacement::Tail,
        vec![WindowPageItem {
            source: crate::kit::history_window::HistorySource::Own,
            entry_id: "s1:m-tool-removed".into(),
            position: Some(10),
            oversize: false,
            sub_items: vec![WindowSubItem {
                position: 0,
                update: serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tool-page-removed",
                    "title": "Read",
                    "rawInput": {},
                }),
                system_reminder: false,
            }],
            descriptor: None,
            descriptor_only: false,
        }],
    );
    assert!(
        crate::kit::acp_events::live_tool_links::owner_for_tool("s1", "tool-page-removed")
            .is_some()
    );
    assert_eq!(state.committed.len(), 1);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let context = WindowRequestContext {
        request_id: "req-open-remove-tool".into(),
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
            response: Box::new(response(identity, Vec::new())),
        },
    );
    assert!(state.committed.is_empty(), "旧页工具卡已被摘除");
    assert!(
        crate::kit::acp_events::live_tool_links::owner_for_tool("s1", "tool-page-removed")
            .is_none(),
        "被摘除的展示单元不得保留 live 归属映射"
    );
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

/// P1 核心场景：纯工具响应轮次只有 ToolStarted/ToolEnded，没有任何文本/推理
/// chunk。工具卡靠服务端 `originMessageId` 直接归属 assistant 条目，进入可淘汰
/// `LiveSeal` 段、不计入 `unattributed`；封印释放后可由历史页重新补读。
#[test]
#[serial]
fn tool_only_turn_origin_message_is_evictable_and_page_refetchable() {
    use crate::kit::history_window::{
        CommitOutcome, HistoryCommitInfo, HistorySource, PersistentEntryKey, PinReason,
        ReleasePlan, ReleaseReason, SegmentOrigin, WindowAnchor, WindowAnchorOffset,
    };
    use peri_acp_types::{
        history_window::HistoryCommittedTail, messages::MessageId, thread::ThreadId,
    };

    let (mut state, _guard) = state_with_window();
    // 视口锚点：释放事务不得回收锚点段。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "anchor body".into(),
            message_id: Some("m-anchor".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // 纯工具轮：没有 TextChunk / ReasoningChunk。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-pure".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({ "file_path": "/tmp/x" }),
            origin_message_id: Some("m-pure".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            tool_id: "tool-pure".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "纯工具轮不得按未归属正文记账"
    );
    let key = PersistentEntryKey::own("s1:m-pure");
    let locator = *state
        .window
        .index()
        .locate_entry(&key)
        .expect("originMessageId 必须建立 assistant 条目归属");
    let segment = state
        .window
        .segment(locator.segment)
        .expect("segment")
        .clone();
    assert_eq!(segment.origin, SegmentOrigin::LiveSeal);
    assert_eq!(locator.unit_count, 1, "只有工具卡");
    assert_eq!(segment.entries[0].position, None);

    // 页回执提升位置 + commit 封印 → 可释放。
    assert!(state.window.promote_live_seal_position(&key, 20));
    state.window.set_anchor(Some(WindowAnchor::new(
        PersistentEntryKey::own("s1:m-anchor"),
        None,
        WindowAnchorOffset::CharOffset(0),
    )));
    let info = HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-1".into(),
        structure_version: 7,
        checkpoint_id: "pure-tool-cp".into(),
        summary_entry_id: None,
        committed_tail: HistoryCommittedTail::new(
            &ThreadId::from("s1".to_string()),
            MessageId::new(),
            20,
        )
        .encode(),
    };
    assert_eq!(
        state.window.apply_commit(&info),
        CommitOutcome::Applied { sealed_segments: 1 }
    );
    let segment = state
        .window
        .segment(locator.segment)
        .expect("sealed")
        .clone();
    assert!(segment.sealed && segment.durable);
    assert!(!segment.pins.contains(PinReason::Unpersisted));

    let plan = ReleasePlan {
        segment: locator.segment,
        source: segment.source.clone(),
        entries: segment.entry_keys(),
        unit_count: segment.unit_count,
        body_bytes: segment.body_bytes,
        reserved_bytes: segment.reserved_bytes,
        slot_range: state.window.index().slot_range(locator.segment),
        before: None,
        after: None,
        before_cursor: None,
        after_cursor: None,
    };
    let released = crate::kit::history_window::release_window_plans(
        &mut state,
        vec![plan],
        ReleaseReason::BudgetPressure,
    );
    assert_eq!(released.released_entries, 1);
    assert_eq!(released.released_units, 1, "纯工具卡可被淘汰");
    assert!(
        !state.committed.iter().any(
            |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-pure")
        ),
        "释放后工具卡离开展示流"
    );
    assert!(state.window.index().locate_entry(&key).is_none());

    // 释放后可补读：同一持久条目由历史页重新接纳，工具卡回到展示流。
    let identity = state.window.identity().cloned().expect("identity");
    let context = context("req-pure-refetch", state.window.generation());
    let page = response(
        identity,
        vec![WindowPageItem {
            source: HistorySource::Own,
            entry_id: "s1:m-pure".into(),
            position: Some(20),
            oversize: false,
            sub_items: vec![WindowSubItem {
                position: 0,
                update: serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tool-pure",
                    "title": "Read",
                    "rawInput": { "file_path": "/tmp/x" },
                }),
                system_reminder: false,
            }],
            descriptor: None,
            descriptor_only: false,
        }],
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );
    assert!(
        state.window.index().locate_entry(&key).is_some(),
        "释放后的持久条目应能由页重新补读"
    );
    assert!(
        state.committed.iter().any(
            |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-pure")
        ),
        "补读后工具卡回到展示流"
    );
}
