//! 复审 10：窗口身份切换与展示/索引的原子协调（自 `history_window_page_test` 拆分）。

use super::*;
use crate::kit::history_window::PersistentEntryKey;

fn open_context(
    request_id: &str,
    identity: &WindowIdentity,
    display_generation: u64,
) -> WindowRequestContext {
    WindowRequestContext {
        request_id: request_id.into(),
        session_id: identity.session_id.clone(),
        identity: identity.clone(),
        window_generation: 0,
        reserved_bytes: 1024,
        placement: WindowPagePlacement::Tail,
        is_open: true,
        display_generation,
        gap: None,
    }
}

fn dispatch_open(
    state: &mut BridgeState,
    request_id: &str,
    identity: WindowIdentity,
    display_generation: u64,
    items: Vec<WindowPageItem>,
) {
    let context = open_context(request_id, &identity, display_generation);
    dispatch_for_bridge(
        state,
        &AcpEventData::HistoryWindowPageLoaded {
            context: Box::new(context),
            response: Box::new(response(identity, items)),
        },
    );
}

#[test]
#[serial]
fn reopen_with_new_turn_keeps_live_answer_once() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );
    assert_eq!(committed_texts(&state), vec!["one"]);

    // open 在途：新 turn 已经 flush 到 committed，current_turn 保持为空。
    let display_generation = state.generation;
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        TuiUserBubble::new("answer".to_string()).with_message_id("m2"),
    ));
    push_view_models(&mut state);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open",
        identity,
        display_generation,
        vec![text_item("m1", 10, "one"), text_item("m2", 20, "answer")],
    );
    push_view_models(&mut state);

    let texts = committed_texts(&state);
    assert_eq!(
        texts,
        vec!["one", "answer"],
        "旧页原位替换，已 flush 的新回答保留一次"
    );
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.as_str() == "answer")
            .count(),
        1,
        "新回答不得被 open 页重复插入"
    );
    let adopted = PersistentEntryKey::own("s1:m2");
    assert!(
        state.window.is_entry_displayed(&adopted),
        "已由 live 尾部展示的条目只登记展示身份"
    );
    assert!(
        !state.window.index().contains_entry(&adopted),
        "adopt 不进入驻留索引，不持有 committed 槽位"
    );
    assert_eq!(state.window.displayed_entry_count(), 2);
    assert_eq!(state.window.segment_count(), 1);

    // 后续翻页重新返回同一持久条目时，adopt 身份必须阻止重复插入。
    load_page(
        &mut state,
        "r2",
        WindowPagePlacement::BeforeFirst,
        vec![text_item("m2", 20, "answer")],
    );
    assert_eq!(
        committed_texts(&state)
            .iter()
            .filter(|text| text.as_str() == "answer")
            .count(),
        1,
        "翻页不得重新插入已 adopt 的条目"
    );
}

#[test]
#[serial]
fn reopen_adopts_only_entries_actually_retained_by_live_tail() {
    use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    // 旧窗口只驻留 m1；更靠后的 m2 已被预算淘汰——committed 与展示账本都
    // 不再持有它，但它仍会在重开页里返回。
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    // 新 turn 完成：live 尾部持有 user 投递（input id = m3）与 assistant
    // 回答（messageId = m4）。
    let display_generation = state.generation;
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        TuiUserBubble::new("question".to_string()).with_message_id("m3"),
    ));
    let mut answer = TuiAssistantBubble {
        text: "answer".to_string(),
        reasoning: None,
        message_id: Some("m4".to_string()),
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    answer.recompute_hash();
    state
        .committed
        .push_back(TuiRenderUnit::TuiAssistantBubble(answer.into()));
    push_view_models(&mut state);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open",
        identity,
        display_generation,
        vec![
            text_item("m1", 10, "one"),
            text_item("m2", 20, "evicted page"),
            text_item("m3", 30, "question"),
            text_item("m4", 40, "answer"),
        ],
    );
    push_view_models(&mut state);

    assert_eq!(
        committed_texts(&state),
        vec!["one", "evicted page", "question", "answer"],
        "被淘汰的历史条目不得因位置高于水位被误判为已展示"
    );
    let evicted = PersistentEntryKey::own("s1:m2");
    assert!(
        state.window.index().contains_entry(&evicted),
        "未被 live 尾部持有的条目必须作为窗口页正文插入"
    );
    assert!(
        state
            .window
            .is_entry_displayed(&PersistentEntryKey::own("s1:m4")),
        "assistant messageId 仍被 live 尾部持有时只登记身份"
    );
}

#[test]
#[serial]
fn reopen_while_streaming_still_adopts_committed_entries() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    // 上一轮已完成的消息仍在 committed：重开页会再次返回同一持久条目。
    let display_generation = state.generation;
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        TuiUserBubble::new("held answer".to_string()).with_message_id("m-held"),
    ));
    push_view_models(&mut state);
    // 新一轮正在流式：live 区域非空。这不应禁用对 committed 已持有条目的
    // adopt，否则重开页会把它们重复插入（复审 CR-2）。
    state.current_turn.append_text("streaming", Some("m-live"));

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open",
        identity,
        display_generation,
        vec![
            text_item("m-fresh", 20, "history"),
            text_item("m-held", 30, "held answer"),
        ],
    );
    push_view_models(&mut state);

    let texts = committed_texts(&state);
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.as_str() == "held answer")
            .count(),
        1,
        "committed 已持有的条目不得因流式区域非空而重复插入"
    );
    assert!(
        texts.iter().any(|text| text.as_str() == "history"),
        "未被持有的条目照常插入"
    );
    assert!(
        state
            .window
            .is_entry_displayed(&PersistentEntryKey::own("s1:m-held")),
        "committed 持有条目只登记展示身份"
    );
    assert!(
        state.current_turn.text.contains("streaming"),
        "窗口切换不得改写正在流式的 live 区域"
    );
}

#[test]
#[serial]
fn adopt_only_ledger_survives_next_identity_switch() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    // 新 turn 已 flush：live 尾部进入 committed。
    let display_generation = state.generation;
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        TuiUserBubble::new("answer".to_string()).with_message_id("m2"),
    ));
    push_view_models(&mut state);

    // 第二个身份只返回已由 live 尾部展示的条目：adopt-only，不建段。
    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    // 模拟 `settle_window_response` 已把 open 响应边界写入 consumer。
    crate::kit::atoms::HISTORY_WINDOW_PAGING
        .state()
        .write()
        .before_cursor = Some("before-2".into());
    dispatch_open(
        &mut state,
        "r-open-2",
        identity,
        display_generation,
        vec![text_item("m2", 20, "answer")],
    );
    push_view_models(&mut state);
    assert_eq!(state.window.displayed_entry_count(), 1);
    assert_eq!(state.window.segment_count(), 0);
    assert_eq!(
        crate::kit::atoms::HISTORY_WINDOW_PAGING
            .state()
            .read()
            .before_cursor
            .as_deref(),
        Some("before-2"),
        "adopt-only open 不得用空布局边缘覆盖响应写入的游标"
    );

    // 再来一次 flush 后，第三个身份返回上一轮回答与新回答。
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        TuiUserBubble::new("third".to_string()).with_message_id("m3"),
    ));
    push_view_models(&mut state);
    let display_generation = state.generation;
    let identity = WindowIdentity::new("s1", "gen-3", 9, "snap-3");
    dispatch_open(
        &mut state,
        "r-open-3",
        identity,
        display_generation,
        vec![text_item("m2", 20, "answer"), text_item("m3", 30, "third")],
    );
    push_view_models(&mut state);

    assert_eq!(committed_texts(&state), vec!["answer", "third"]);
    assert_eq!(
        state.window.displayed_entry_count(),
        2,
        "adopt 身份在再次身份切换后仍须保留"
    );
    assert_eq!(state.window.segment_count(), 0, "adopt-only 链不物化正文段");
}

#[test]
#[serial]
fn adopt_only_reopen_places_older_history_before_live_tail() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    let display_generation = state.generation;
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        TuiUserBubble::new("answer".to_string()).with_message_id("m2"),
    ));
    push_view_models(&mut state);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open-2",
        identity,
        display_generation,
        vec![text_item("m2", 20, "answer")],
    );
    push_view_models(&mut state);

    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        TuiUserBubble::new("third".to_string()).with_message_id("m3"),
    ));
    push_view_models(&mut state);
    let display_generation = state.generation;
    let identity = WindowIdentity::new("s1", "gen-3", 9, "snap-3");
    dispatch_open(
        &mut state,
        "r-open-3",
        identity,
        display_generation,
        vec![
            text_item("m1", 10, "one"),
            text_item("m2", 20, "answer"),
            text_item("m3", 30, "third"),
        ],
    );
    push_view_models(&mut state);

    assert_eq!(
        committed_texts(&state),
        vec!["one", "answer", "third"],
        "adopt-only 之后重取的历史必须插到 live 尾部之前"
    );
}

/// 流式 turn 期间重开：账本只有 adopt 身份时，open 页的更早历史仍须插到
/// live 尾部之前，不能因为 `live_empty == false` 而追加到末尾。
#[test]
#[serial]
fn streaming_reopen_places_older_history_before_live_tail() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    let display_generation = state.generation;
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        TuiUserBubble::new("answer".to_string()).with_message_id("m2"),
    ));
    push_view_models(&mut state);

    // adopt-only：window 只登记身份，不建段；committed 保留 live 尾部。
    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open-2",
        identity,
        display_generation,
        vec![text_item("m2", 20, "answer")],
    );
    push_view_models(&mut state);
    assert_eq!(state.window.displayed_entry_count(), 1);
    assert_eq!(state.window.segment_count(), 0);

    // 新 turn 正在流式输出：live 区域非空，adopt 不启用。
    state.current_turn.append_text("streaming", Some("m-live"));
    let display_generation = state.generation;
    let identity = WindowIdentity::new("s1", "gen-3", 9, "snap-3");
    dispatch_open(
        &mut state,
        "r-open-3",
        identity,
        display_generation,
        vec![text_item("m0", 5, "zero")],
    );
    push_view_models(&mut state);

    assert_eq!(
        committed_texts(&state),
        vec!["zero", "answer"],
        "更早的历史必须插到 live 尾部之前"
    );
}

#[test]
#[serial]
fn reopen_after_streaming_turn_does_not_duplicate_prior_page() {
    use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );

    let display_generation = state.generation;
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            "new question".to_string(),
        )));
    push_view_models(&mut state);
    state.current_turn.append_text("streaming", Some("m-live"));

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    dispatch_open(
        &mut state,
        "r-open",
        identity,
        display_generation,
        vec![text_item("m2", 20, "two")],
    );
    push_view_models(&mut state);

    assert_eq!(
        committed_texts(&state),
        vec!["two", "new question"],
        "旧窗口页被精确摘除，live 尾部保留，open 页插到 live 之前"
    );
    assert!(
        state.current_turn.text.contains("streaming"),
        "窗口切换不得改写正在流式的 live 区域"
    );

    load_page(
        &mut state,
        "r-before",
        WindowPagePlacement::BeforeFirst,
        vec![text_item("m1", 10, "one")],
    );
    assert_eq!(
        committed_texts(&state)
            .iter()
            .filter(|text| text.as_str() == "one")
            .count(),
        1,
        "旧 committed 不得因身份切换失去去重保护"
    );
}

#[test]
#[serial]
fn same_identity_open_retry_preserves_display_and_dedupes() {
    let (mut state, _guard) = state_with_window();
    load_page(
        &mut state,
        "r1",
        WindowPagePlacement::Tail,
        vec![text_item("m1", 10, "one")],
    );
    state.window.set_degraded("cursor expired");
    let identity = state.window.identity().cloned().expect("identity");
    let display_generation = state.generation;

    dispatch_open(
        &mut state,
        "r-open-same",
        identity,
        display_generation,
        vec![text_item("m1", 10, "one"), text_item("m2", 20, "two")],
    );
    push_view_models(&mut state);

    assert_eq!(committed_texts(&state), vec!["one", "two"]);
    assert_eq!(state.window.displayed_entry_count(), 2);
    assert_eq!(
        state.window.segment_count(),
        2,
        "旧段保留，重试只合并新页（m1 被展示身份去重）"
    );
    assert!(!state.window.is_degraded(), "成功的 open 必须恢复 Ready");
    assert!(
        !crate::kit::atoms::HISTORY_WINDOW_PAGING
            .state()
            .read()
            .is_degraded(),
        "consumer 也必须在 open 成功后离开 Degraded"
    );
}

fn committed_tool_cards(state: &BridgeState) -> Vec<String> {
    state
        .committed
        .iter()
        .filter_map(|unit| match unit {
            crate::kit::tui_render_unit::TuiRenderUnit::TuiToolCard(card) => {
                Some(card.tool_id.clone())
            }
            _ => None,
        })
        .collect()
}

/// 同身份 open 重试不得重复插入 live 工具卡。
///
/// open 页按 `tool_id` 给出 live 卡片的持久身份时，adopt/回填必须与本次 open
/// 是否切换身份无关：第一次同身份 open 就要把页条目登记为已展示并回填展示槽位，
/// 重试只按 `tool_id` 去重。先前只在 `identity_changed` 分支启用 adopt，导致页
/// 工具子项被当成新正文插入，同一 `tool_id` 出现两张卡片，且 live 卡片始终留在
/// 未归属账目里。
#[test]
#[serial]
fn same_identity_open_retry_does_not_duplicate_tool_cards() {
    use crate::kit::history_window::{
        CommitOutcome, HistoryCommitInfo, PersistentEntryKey, ReleaseReason,
    };
    use peri_acp_types::history_window::HistoryCommittedTail;
    use peri_acp_types::messages::MessageId;
    use peri_acp_types::thread::ThreadId;

    let (mut state, _guard) = state_with_window();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "anchor".into(),
            message_id: Some("m-anchor".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    // 清掉流式启发式身份：工具卡按无归属正文记账，等待 open 页回填。
    crate::kit::acp_events::live_tool_links::note_turn_boundary("s1");
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-live".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(
        state.window.budget().unattributed_live_bytes > 0,
        "无身份工具卡先按未归属记账"
    );

    let anchor_key = PersistentEntryKey::own("s1:m-anchor");
    let tool_key = PersistentEntryKey::own("s1:m-tool-page");
    let identity = state.window.identity().cloned().expect("identity");
    let display_generation = state.generation;
    let page_item = || WindowPageItem {
        source: crate::kit::history_window::HistorySource::Own,
        entry_id: "s1:m-tool-page".into(),
        position: Some(10),
        oversize: false,
        sub_items: vec![WindowSubItem {
            position: 0,
            update: serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": "tool-live",
                "title": "Read",
                "rawInput": { "file_path": "/tmp/x" },
            }),
            system_reminder: false,
        }],
        descriptor: None,
        descriptor_only: false,
    };

    dispatch_open(
        &mut state,
        "r-open-same-1",
        identity.clone(),
        display_generation,
        vec![page_item()],
    );
    push_view_models(&mut state);
    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "回填后未归属账目必须回落"
    );
    assert_eq!(
        committed_tool_cards(&state),
        vec!["tool-live"],
        "同身份 open 首次到达就必须按 tool_id adopt，不能插入第二张卡"
    );
    assert_eq!(state.window.displayed_range(&anchor_key), Some((0, 1)));
    assert_eq!(
        state.window.displayed_range(&tool_key),
        Some((1, 1)),
        "live 工具卡必须获得与 committed 一致的展示槽位"
    );
    assert_eq!(state.window.segment_count(), 2);

    let retry_generation = state.generation;
    dispatch_open(
        &mut state,
        "r-open-same-2",
        identity,
        retry_generation,
        vec![page_item()],
    );
    push_view_models(&mut state);
    assert_eq!(
        committed_tool_cards(&state),
        vec!["tool-live"],
        "同身份重试不得重复插入同一 tool_id"
    );
    assert_eq!(
        state.window.budget().unattributed_live_bytes,
        0,
        "重试不得把已归属卡片重新计入未归属"
    );
    assert_eq!(
        state.window.displayed_range(&tool_key),
        Some((1, 1)),
        "重试不得重复登记展示槽位"
    );
    assert_eq!(state.window.segment_count(), 2, "重试不得重复入段");

    // 封印工具段后按真实淘汰计划释放：只删除一次，锚点文本保留。
    let info = HistoryCommitInfo {
        session_id: "s1".into(),
        session_generation: "gen-1".into(),
        structure_version: 7,
        checkpoint_id: "cp-same-identity".into(),
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
    let body_limit = state.window.budget().limits.body_bytes;
    let plans = state.window.plan_eviction(body_limit.saturating_mul(2));
    let tool_plan = plans
        .plans
        .into_iter()
        .find(|plan| plan.entries.contains(&tool_key))
        .expect("工具段是唯一 durable 的可淘汰候选");
    assert_eq!(tool_plan.unit_count, 1, "同一 tool_id 只对应一个槽位");
    let outcome = crate::kit::history_window::release_window_plans(
        &mut state,
        vec![tool_plan],
        ReleaseReason::BudgetPressure,
    );
    assert_eq!(outcome.released_units, 1, "释放只能删除一次");
    assert!(committed_tool_cards(&state).is_empty());
    assert!(
        state
            .committed
            .iter()
            .any(|unit| matches!(unit, TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "anchor")),
        "锚点文本不得被工具段释放牵连"
    );
}

/// 同一条目同时含文本与 live 工具卡时，adopt 只能跳过工具子项，兄弟文本仍须入页。
///
/// 整条条目 adopt 会把不在 `committed` 的文本标记为已展示且不插入正文，随后被
/// 展示身份去重永久挡住；这里锁定按单元 adopt 的行为。
#[test]
#[serial]
fn same_identity_open_keeps_page_text_around_adopted_tool_card() {
    use crate::kit::history_window::PersistentEntryKey;

    let (mut state, _guard) = state_with_window();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "anchor".into(),
            message_id: Some("m-anchor".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    // 清掉流式启发式身份：工具卡按无归属正文记账，等待 open 页回填。
    crate::kit::acp_events::live_tool_links::note_turn_boundary("s1");
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "tool-mixed".into(),
            tool_name: "Read".into(),
            input_summary: "file".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    let identity = state.window.identity().cloned().expect("identity");
    let display_generation = state.generation;
    let mixed_key = PersistentEntryKey::own("s1:m-mixed");
    let mixed_item = WindowPageItem {
        source: crate::kit::history_window::HistorySource::Own,
        entry_id: "s1:m-mixed".into(),
        position: Some(10),
        oversize: false,
        sub_items: vec![
            WindowSubItem {
                position: 0,
                update: serde_json::json!({
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "page text" },
                    "messageId": "m-mixed",
                }),
                system_reminder: false,
            },
            WindowSubItem {
                position: 1,
                update: serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tool-mixed",
                    "title": "Read",
                    "rawInput": { "file_path": "/tmp/x" },
                }),
                system_reminder: false,
            },
        ],
        descriptor: None,
        descriptor_only: false,
    };

    dispatch_open(
        &mut state,
        "r-open-mixed",
        identity,
        display_generation,
        vec![mixed_item],
    );
    push_view_models(&mut state);

    assert_eq!(
        committed_tool_cards(&state),
        vec!["tool-mixed"],
        "被 live 持有的工具子项不得重复插入"
    );
    let texts = committed_texts(&state);
    assert!(
        texts.contains(&"page text".to_string()),
        "同条目的兄弟文本必须随页插入，不能被整条 adopt 丢弃：{texts:?}"
    );
    // 工具卡仍按未归属正文记账：页条目已作为页段驻留，LiveSeal 不能并入同一
    // 身份（`register_live_seal_segments` 的既有保守路径）。这是本修复保留的
    // 残余风险，不得反过来丢弃兄弟文本。
    assert!(
        state.window.budget().unattributed_live_bytes > 0,
        "混合条目的 live 工具卡按既有保守路径继续计入未归属"
    );
    let ranges = state.window.display.debug_ranges();
    assert!(
        ranges
            .iter()
            .any(|(start, key, len)| key == &mixed_key && (*start, *len) == (2, 1)),
        "页文本必须获得自己的展示槽位：{ranges:?}"
    );
}

#[test]
#[serial]
fn reopen_with_inconsistent_ledger_degrades_without_mutation() {
    let (mut state, _guard) = state_with_window();
    let identity_before = state.window.identity().cloned().expect("identity");
    let generation_before = state.window.generation();
    // 账本区间远超 committed：不得猜测性删除。
    state
        .window
        .mark_display_block(&[(PersistentEntryKey::own("s1:broken"), 2)], 100);

    let identity = WindowIdentity::new("s1", "gen-2", 8, "snap-2");
    let display_generation = state.generation;
    dispatch_open(
        &mut state,
        "r-open-broken",
        identity,
        display_generation,
        vec![text_item("m1", 10, "one")],
    );

    assert_eq!(
        state.window.identity(),
        Some(&identity_before),
        "失败不得切换窗口身份"
    );
    assert_eq!(
        state.window.generation(),
        generation_before,
        "失败不得推进窗口代次"
    );
    assert!(state.committed.is_empty(), "失败不得修改 committed");
    assert_eq!(
        state.window.displayed_entry_count(),
        1,
        "原账本保持原样，等待会话边界 reset"
    );
    assert!(state.window.is_degraded(), "失败必须显式进入 Degraded");
    let consumer = crate::kit::atoms::HISTORY_WINDOW_PAGING
        .state()
        .read()
        .clone();
    assert!(consumer.is_degraded(), "consumer 必须等待 open 重试");
    assert!(consumer.pending_merge.is_none());
}
