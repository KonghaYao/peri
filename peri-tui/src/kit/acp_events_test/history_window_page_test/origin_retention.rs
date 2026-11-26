//! Origin retention regression scenarios.

use super::AcpEventData;
use super::HistoryWindow;
use super::HistoryWindowConfig;
use super::SessionPhase;
use super::TuiRenderUnit;
use super::WindowIdentity;
use super::WindowPageItem;
use super::WindowSubItem;
use super::context;

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
    use peri_acp_types::history_window::HistoryCommittedTail;
    use peri_acp_types::messages::MessageId;
    use peri_acp_types::thread::ThreadId;

    let (mut state, _guard) = super::state_with_window();
    // 视口锚点：释放事务不得回收锚点段。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "anchor body".into(),
            message_id: Some("m-anchor".into()),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // 纯工具轮：没有 TextChunk / ReasoningChunk。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-pure".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({ "file_path": "/tmp/x" }),
            origin_message_id: Some("m-pure".into()),
            origin_readable: Some(true),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            images: Vec::new(),
            tool_id: "tool-pure".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

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
    let page = super::response(
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
    super::dispatch_and_notify(
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

/// P1 阻塞回归：嵌套执行显式声明 `originReadable=false`。该卡不得回退到外层
/// assistant 身份，不进入可淘汰段；外层记录被 commit + 预算压力淘汰后，嵌套卡
/// 仍留在展示流（页补读只能恢复外层记录）。
#[test]
#[serial]
fn nested_tool_without_readable_origin_is_not_evicted_with_outer_message() {
    use crate::kit::history_window::{
        CommitOutcome, HistoryCommitInfo, PersistentEntryKey, ReleaseReason, WindowAnchor,
        WindowAnchorOffset, WindowLimits,
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
    state.current_request_id = Some("req-1".into());
    state.turn_generation = 9;

    // 外层文本建立 current_message_id；嵌套卡紧随其后且显式不可回读；
    // 顶层工具卡携带真实可回读 origin。三者同一轮归档。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "outer body".into(),
            message_id: Some("m-outer".into()),
            agent_id: None,
        }),
    );
    let nested_summary = "n".repeat(300);
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-nested".into(),
            tool_name: "Read".into(),
            input_summary: nested_summary.clone(),
            raw_input: serde_json::json!({"file_path": "/tmp/nested.rs"}),
            origin_message_id: None,
            origin_readable: Some(false),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            images: Vec::new(),
            tool_id: "tool-nested".into(),
            output_summary: "nested output".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-top".into(),
            tool_name: "Read".into(),
            input_summary: "x".repeat(8 * 1024),
            raw_input: serde_json::json!({}),
            origin_message_id: Some("m-outer".into()),
            origin_readable: Some(true),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            images: Vec::new(),
            tool_id: "tool-top".into(),
            output_summary: "top output".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // 1) 嵌套卡没有任何归属；字节计入未归属预算。
    assert!(
        crate::kit::acp_events::live_tool_links::owner_for_tool("s1", "tool-nested").is_none(),
        "Some(false) 的嵌套卡不得回退到外层消息身份"
    );
    assert!(
        state.window.budget().unattributed_live_bytes >= nested_summary.len(),
        "嵌套卡正文必须计入未归属预算，实际 {}",
        state.window.budget().unattributed_live_bytes
    );

    // 2) 外层消息 + 顶层工具卡进入同一个可淘汰段。
    let key = PersistentEntryKey::own("s1:m-outer");
    let locator = *state
        .window
        .index()
        .locate_entry(&key)
        .expect("顶层工具卡归属外层消息");
    assert_eq!(
        state.window.segment(locator.segment).expect("nested tool without readable origin is not evicted with outer message: the segment fixture operation uses valid inputs").unit_count,
        2,
        "外层段只含文本与顶层工具卡，不得包含嵌套卡"
    );
    assert!(state.window.promote_live_seal_position(&key, 10));

    let info = HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-1".into(),
        structure_version: 7,
        checkpoint_id: "nested-cp".into(),
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

    // 3) 预算压力下的淘汰计划只能选中外层段，嵌套卡没有可释放段。
    let plan = state.window.plan_eviction(0);
    assert!(
        plan.plans.iter().any(|plan| plan.entries.contains(&key)),
        "外层段应成为淘汰候选"
    );
    assert!(
        plan.plans
            .iter()
            .flat_map(|plan| plan.entries.iter())
            .all(|entry| !entry.entry_id.contains("tool-nested")),
        "嵌套卡不得进入任何释放计划"
    );
    let released = crate::kit::history_window::release_window_plans(
        &mut state,
        plan.plans,
        ReleaseReason::BudgetPressure,
    );
    assert_eq!(released.released_entries, 1);
    // 渲染侧 ack 归还保守额度，页接纳才能重新获得预算容量。
    state.window.ack_release_epoch(released.structural_epoch);
    assert!(
        state.window.budget().unattributed_live_bytes >= nested_summary.len(),
        "释放外层段不得吞掉嵌套卡的未归属字节"
    );
    assert!(
        !state.committed.iter().any(
            |unit| matches!(unit, TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.message_id.as_deref() == Some("m-outer"))
        ),
        "外层正文已随段释放"
    );
    assert!(
        !state.committed.iter().any(
            |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-top")
        ),
        "可回读的顶层工具卡随段释放"
    );
    assert!(
        state.committed.iter().any(
            |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-nested")
        ),
        "嵌套卡不得随外层段一起消失"
    );

    // 4) 页接纳只恢复外层记录；嵌套卡仍在展示流。
    let context = context("req-nested-refetch", state.window.generation());
    let page = super::response(
        state.window.identity().cloned().expect("identity"),
        vec![super::text_item("m-outer", 10, "outer body")],
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(page),
        },
    );
    assert!(
        state.window.index().locate_entry(&key).is_some(),
        "页补读恢复外层记录"
    );
    assert!(
        state.committed.iter().any(
            |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-nested")
        ),
        "页补读后嵌套卡仍保留（不能只恢复外层记录）"
    );

    // 5) 上滚换锚并再次计算保留计划：嵌套卡仍不是可释放对象。
    state.window.set_anchor(Some(WindowAnchor::new(
        key.clone(),
        None,
        WindowAnchorOffset::CharOffset(0),
    )));
    let scroll_plans = state.window.retention_plan_after_commit();
    assert!(
        scroll_plans
            .iter()
            .flat_map(|plan| plan.entries.iter())
            .all(|entry| !entry.entry_id.contains("tool-nested")),
        "上滚后的保留计划也不得选中嵌套卡（它没有持久段）"
    );
    assert!(
        state.committed.iter().any(
            |unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "tool-nested")
        ),
        "上滚后嵌套卡仍保留"
    );
}

use serial_test::serial;
