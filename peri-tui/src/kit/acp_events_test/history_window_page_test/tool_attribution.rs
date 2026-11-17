//! Tool attribution regression scenarios.

use super::AcpEventData;
use super::HistoryWindow;
use super::SessionPhase;
use super::TuiRenderUnit;
use super::WindowIdentity;
use super::WindowPageItem;
use super::WindowPagePlacement;
use super::WindowRequestContext;
use super::WindowSubItem;
use super::context;
use super::response;

/// 工具卡没有 live `messageId`：open 页按 `tool_id` 给出持久条目后，未归属卡片
/// 补登记为可淘汰段，`unattributed_live_bytes` 同时回落。
#[test]
#[serial]
fn open_page_binds_unattributed_tool_card_to_page_entry() {
    let (mut state, _guard) = super::state_with_window();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "anchor body".into(),
            message_id: Some("m-anchor".into()),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    crate::kit::acp_events::live_tool_links::note_turn_boundary("s1");
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-page".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
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
    super::dispatch_for_bridge(
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

    let (mut state, _guard) = super::state_with_window();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-late".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    crate::kit::acp_events::live_tool_links::note_turn_boundary("s1");
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "late text".into(),
            message_id: Some("m-late".into()),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

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
            super::text_item("m-older", 5, "older body"),
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
    super::dispatch_for_bridge(
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
        state.window.segment(tool_locator.segment).expect("open page backfill keeps trailing text slot and layout order: the segment fixture operation uses valid inputs").origin,
        SegmentOrigin::LiveSeal
    );
    let tool_segment = state.window.segment(tool_locator.segment).expect("open page backfill keeps trailing text slot and layout order: the segment fixture operation uses valid inputs").clone();
    let text_segment = state.window.segment(text_locator.segment).expect("open page backfill keeps trailing text slot and layout order: the segment fixture operation uses valid inputs").clone();

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

    let (mut state, _guard) = super::state_with_window();
    let limit = state.window.budget().limits.body_bytes;
    state.window.note_unattributed_live_bytes(limit);
    let rejected_identity = state.window.identity().cloned().expect("identity");
    let rejected_generation = state.window.generation();
    super::dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context("req-rejected", rejected_generation)),
            response: Box::new(response(
                rejected_identity,
                vec![super::text_item("m-first", 10, "page body")],
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
    super::dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context("req-accepted", accepted_generation)),
            response: Box::new(response(
                identity,
                vec![super::text_item("m-second", 20, "page body")],
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
    let _guard = super::setup_atoms(&identity);
    let mut state = super::make_fold_test_state();
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
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "anchor".into(),
            message_id: Some("m-anchor".into()),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    crate::kit::acp_events::live_tool_links::note_turn_boundary("s1");
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-big".into(),
            tool_name: "Read".into(),
            input_summary: "x".repeat(8 * 1024),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
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
            super::text_item("m-anchor", 5, "anchor"),
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
    super::dispatch_for_bridge(
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
    let page = response(
        page_identity,
        vec![super::text_item("m-new", 20, "new page body")],
    );
    super::dispatch_for_bridge(
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
    let (mut state, _guard) = super::state_with_window();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "attributed body".into(),
            message_id: Some("m-attr".into()),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-attr".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let key = crate::kit::history_window::PersistentEntryKey::own("s1:m-attr");
    let locator = *state.window.index().locate_entry(&key).expect("live entry");
    let before_units = locator.unit_count;
    let before_body = state.window.segment(locator.segment).expect("open page does not extend already attributed tool card: the segment fixture operation uses valid inputs").body_bytes;
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
    super::dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response),
        },
    );

    let locator = *state.window.index().locate_entry(&key).expect("live entry");
    assert_eq!(locator.unit_count, before_units, "no duplicate unit merge");
    assert_eq!(
        state.window.segment(locator.segment).expect("open page does not extend already attributed tool card: the segment fixture operation uses valid inputs").body_bytes,
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

/// rewind 替换整段 `committed`：live 工具归属必须一起丢弃，避免长进程累积。
#[test]
#[serial]
fn rewind_forgets_live_tool_links() {
    let (mut state, _guard) = super::state_with_window();
    crate::kit::acp_events::live_tool_links::note_root_tool_started(
        "s1",
        "tool-rewind",
        Some("m-rewind"),
        None,
    );
    assert!(crate::kit::acp_events::live_tool_links::owner_for_tool("s1", "tool-rewind").is_some());

    super::dispatch_for_bridge(
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
    let (mut state, _guard) = super::state_with_window();
    crate::kit::acp_events::live_tool_links::note_root_tool_started(
        "s1",
        "tool-page-removed",
        Some("m-origin"),
        None,
    );
    super::load_page(
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
    super::dispatch_for_bridge(
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

use serial_test::serial;
