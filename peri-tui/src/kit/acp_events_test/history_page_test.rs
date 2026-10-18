//! 历史页并入窗口的验收级用例（REQ-C01、A20/A23/A24）。
//!
//! 覆盖：区间合并（整页更早 / 中间填补）、按 `entryId` 去重（重试同一页）、
//! 按「会话 + 快照」丢弃迟到页、投影隔离（不碰执行 / 输入 / Todo 状态）、
//! 跨页的工具起点-终点配对。

use super::*;
use crate::kit::history_paging::{self, HistoryPageItem, HistoryPageResponse, HistorySubItem};
use serde_json::json;

#[path = "history_page_test/over_budget_tool_result.rs"]
mod over_budget_tool_result;

fn sub(position: u64, update: serde_json::Value) -> HistorySubItem {
    HistorySubItem {
        position,
        update,
        system_reminder: false,
    }
}

fn page(
    token: &str,
    cursor: Option<&str>,
    items: Vec<(&str, u64, Vec<HistorySubItem>)>,
) -> HistoryPageResponse {
    HistoryPageResponse {
        session_id: "s1".to_string(),
        snapshot_token: token.to_string(),
        next_cursor: cursor.map(str::to_owned),
        items: items
            .into_iter()
            .map(|(entry_id, position, sub_items)| HistoryPageItem {
                entry_id: entry_id.to_string(),
                position,
                oversize: false,
                sub_items,
            })
            .collect(),
    }
}

fn text_update(text: &str) -> serde_json::Value {
    json!({
        "sessionUpdate": "agent_message_chunk",
        "content": { "type": "text", "text": text },
        "messageId": "m",
    })
}

fn user_update(text: &str) -> serde_json::Value {
    json!({
        "sessionUpdate": "user_message_chunk",
        "content": { "type": "text", "text": text },
        "messageId": "m",
    })
}

fn tool_start(tool_id: &str, tool_name: &str) -> serde_json::Value {
    json!({
        "sessionUpdate": "tool_call",
        "toolCallId": tool_id,
        "title": tool_name,
        "rawInput": { "file_path": "/tmp/x" },
    })
}

fn tool_end(tool_id: &str, output: &str, is_error: bool) -> serde_json::Value {
    json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": tool_id,
        "rawOutput": output,
        "status": if is_error { "failed" } else { "completed" },
    })
}

/// 当前浏览身份（会话 `s1` + 快照 `tok`）与空窗口。
fn browsing_state(token: &str) -> BridgeState {
    crate::kit::atoms::init_atoms();
    history_paging::reset_browsing_state();
    *crate::kit::atoms::ACTIVE_SESSION_ID.state().write() = "s1".to_string();
    let mut state = make_fold_test_state();
    state.active_session_id = "s1".to_string();
    history_paging::note_initial_cursor(
        "s1",
        Some(token.to_string()),
        Some("cursor-1".to_string()),
    );
    state
}

/// 派发一页，并要求它**必须落地**（不是被丢弃的迟到页）。
///
/// 浏览身份（`atoms::HISTORY_PAGING`）与区间索引都是进程级共享的：并行运行的非
/// `serial` 用例（会话 load / 切会话 / rewind 走 `project_session_boundary`、
/// 渲染路径的滚动触发）会写同一份状态，页面随之被判为迟到页丢弃——本文件的断言
/// 会因此变成随机失败。派发前把身份钉回本用例，并在真的丢了身份时明确失败。
fn dispatch_landing_page(state: &mut BridgeState, token: &str, response: HistoryPageResponse) {
    {
        let handle = crate::kit::atoms::HISTORY_PAGING.state();
        let mut paging = handle.write();
        paging.session_id = "s1".to_string();
        paging.snapshot_token = Some(token.to_string());
    }
    assert!(
        crate::kit::atoms::HISTORY_PAGING
            .state()
            .read()
            .accepts("s1", token),
        "浏览身份在派发前被并行用例改写（测试间共享 atom 干扰）"
    );
    dispatch_for_bridge(
        state,
        &AcpEventData::HistoryPageLoaded {
            response: Box::new(response),
        },
    );
}

/// 失败诊断：把当前浏览身份与区间索引写进断言消息——用来区分「页被丢弃」与
/// 「页并进了错误的区间」。
fn paging_probe() -> String {
    let paging = crate::kit::atoms::HISTORY_PAGING.state().read().clone();
    let spans = history_paging::with_spans(|spans| format!("{spans:?}"));
    format!("paging={paging:?} spans={spans}")
}

/// 让首屏自带一条自有条目（回放标记 + 一条 user 气泡），返回它的槽位。
fn seed_first_screen_own_entry(state: &mut BridgeState, entry_id: &str, position: u64) -> usize {
    dispatch_for_bridge(
        state,
        &AcpEventData::ReplayEntryMarked {
            entry_id: entry_id.to_string(),
            position,
        },
    );
    let slot = state.committed.len();
    dispatch_for_bridge(
        state,
        &AcpEventData::ReplayedUserBubble {
            input_id: format!("input-{entry_id}"),
            text: format!("first-screen-{entry_id}"),
        },
    );
    dispatch_for_bridge(state, &AcpEventData::SessionReplayDone);
    slot
}

fn unit_texts(state: &BridgeState) -> Vec<String> {
    state
        .committed
        .iter()
        .map(|unit| match unit {
            TuiRenderUnit::TuiUserBubble(bubble) => bubble.text.clone(),
            TuiRenderUnit::TuiAssistantBubble(bubble) => bubble.text.clone(),
            TuiRenderUnit::TuiToolCard(card) => format!("tool:{}", card.output_summary),
            other => format!("{other:?}"),
        })
        .collect()
}

/// 整页更早：插在继承前缀之后、首屏自有条目之前（REQ-H01-6、A20）。
#[test]
#[serial]
fn test_history_page_prepends_older_page_after_inherited_prefix() {
    let mut state = browsing_state("tok");
    // 继承前缀：无 position 的祖先前缀（没有回放标记，因此不进区间索引）。
    state.committed = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
        "inherited".to_string(),
    ))]);
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
    let epoch_before = crate::kit::atoms::HISTORY_PREPEND_EPOCH.get();

    let response = page(
        "tok",
        Some("cursor-2"),
        vec![
            ("s1:old-1", 10, vec![sub(0, user_update("old one"))]),
            ("s1:old-2", 20, vec![sub(0, text_update("old two"))]),
        ],
    );
    dispatch_landing_page(&mut state, "tok", response);

    let texts = unit_texts(&state);
    assert_eq!(texts.len(), 4, "{texts:?} {}", paging_probe());
    assert_eq!(texts[0], "inherited");
    assert_eq!(texts[1], "old one");
    assert_eq!(texts[2], "old two");
    assert_eq!(texts[3], "first-screen-s1:own-latest");
    assert_eq!(
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.get(),
        epoch_before + 1,
        "并入成功必须推进锚点补偿代数"
    );
}

/// 生产形状：**没有**回放结束通知（`SessionReplayDone` 是死路径），首屏最后一个
/// 自有条目仍处于「打开」状态。整页更早的历史仍必须落在继承前缀之后（REQ-H01-6、A20）。
#[test]
#[serial]
fn test_history_page_prepends_after_prefix_without_replay_done() {
    let mut state = browsing_state("tok");
    state.committed = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
        "inherited".to_string(),
    ))]);
    // 只发标记与单元，不发 SessionReplayDone。
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayEntryMarked {
            entry_id: "s1:own-latest".to_string(),
            position: 100,
        },
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayedUserBubble {
            input_id: "input-own".to_string(),
            text: "first-screen-own".to_string(),
        },
    );

    let response = page(
        "tok",
        None,
        vec![("s1:old-1", 10, vec![sub(0, user_update("old one"))])],
    );
    dispatch_landing_page(&mut state, "tok", response);

    assert_eq!(
        unit_texts(&state),
        vec!["inherited", "old one", "first-screen-own"],
        "更早的 own 历史不得越过继承前缀；{}",
        paging_probe()
    );
}

/// 重试同一页：按 `entryId` 去重，不重复插入（REQ-C01-2 幂等）。
#[test]
#[serial]
fn test_history_page_dedupes_repeated_page() {
    let mut state = browsing_state("tok");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
    let response = page(
        "tok",
        Some("cursor-2"),
        vec![("s1:old-1", 10, vec![sub(0, user_update("old one"))])],
    );
    dispatch_landing_page(&mut state, "tok", response.clone());
    let len_after_first = state.committed.len();
    let epoch_after_first = crate::kit::atoms::HISTORY_PREPEND_EPOCH.get();

    dispatch_landing_page(&mut state, "tok", response);

    assert_eq!(state.committed.len(), len_after_first);
    assert_eq!(
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.get(),
        epoch_after_first,
        "重复页不产生锚点补偿"
    );
}

/// 中间填补：整页落在两个已载入条目之间（稀疏首屏的空缺）。
#[test]
#[serial]
fn test_history_page_fills_gap_between_loaded_entries() {
    let mut state = browsing_state("tok");
    // 首屏：own-a(position 10) 与 own-b(position 100)。
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayEntryMarked {
            entry_id: "s1:own-a".to_string(),
            position: 10,
        },
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayedUserBubble {
            input_id: "input-a".to_string(),
            text: "own-a".to_string(),
        },
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayEntryMarked {
            entry_id: "s1:own-b".to_string(),
            position: 100,
        },
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::CommittedAssistantText {
            text: "own-b".to_string(),
            reasoning: None,
        },
    );
    dispatch_for_bridge(&mut state, &AcpEventData::SessionReplayDone);

    // 页 1 早于 own-a；页 2 落在 own-a 与 own-b 之间。
    let newer = page(
        "tok",
        Some("cursor-2"),
        vec![("s1:mid", 50, vec![sub(0, text_update("between"))])],
    );
    dispatch_landing_page(&mut state, "tok", newer);
    let older = page(
        "tok",
        None,
        vec![("s1:old", 5, vec![sub(0, user_update("before-all"))])],
    );
    dispatch_landing_page(&mut state, "tok", older);

    assert_eq!(
        unit_texts(&state),
        vec!["before-all", "own-a", "between", "own-b"],
        "{}",
        paging_probe()
    );
}

/// 迟到页丢弃：快照身份不一致（切会话 / 重新 load / rewind 之后）整页不落地。
#[test]
#[serial]
fn test_history_page_drops_page_from_stale_snapshot() {
    let mut state = browsing_state("tok-current");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
    let before = unit_texts(&state);
    let epoch_before = crate::kit::atoms::HISTORY_PREPEND_EPOCH.get();

    let stale = page(
        "tok-stale",
        None,
        vec![("s1:old", 10, vec![sub(0, text_update("stale"))])],
    );
    // 刻意不走 `dispatch_landing_page`：本用例要的正是「身份不一致 → 页被丢」。
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryPageLoaded {
            response: Box::new(stale),
        },
    );

    assert_eq!(unit_texts(&state), before);
    assert_eq!(
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.get(),
        epoch_before,
        "丢弃的页不得推进锚点补偿代数"
    );
}

/// 投影隔离：有 turn 在跑、有排队输入、有 Todo 基线时翻页不得推进任何执行状态
/// （REQ-C01-6、A24）。
#[test]
#[serial]
fn test_history_page_does_not_disturb_execution_state() {
    let mut state = browsing_state("tok");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
    state.phase = SessionPhase::PromptRunning;
    state.turn_generation = 7;
    state.last_submitted_text = Some(SubmittedInputRollback {
        text: "pending".to_string(),
        request_id: Some("rid".to_string()),
    });
    state.current_turn.append_text("streaming", Some("m1"));
    *crate::kit::atoms::INPUT_BUFFER.state().write() =
        std::collections::VecDeque::from([crate::kit::atoms::BufferedInput::text("queued input")]);
    *crate::kit::atoms::TODO_ITEMS.state().write() = vec![];
    // 审批/问答弹窗与 pending 交互也必须原样保留（REQ-C01-6、A23：历史页不发起审批）。
    *crate::kit::atoms::HITL_PENDING.state().write() = None;
    *crate::kit::atoms::POPUP_KIND.state().write() = None;

    let response = page(
        "tok",
        None,
        vec![
            ("s1:old-1", 10, vec![sub(0, text_update("old text"))]),
            ("s1:old-2", 20, vec![sub(0, tool_start("t-old", "Read"))]),
        ],
    );
    dispatch_landing_page(&mut state, "tok", response);

    assert_eq!(state.phase, SessionPhase::PromptRunning);
    assert_eq!(state.turn_generation, 7);
    assert_eq!(
        state.last_submitted_text.as_ref().map(|r| r.text.as_str()),
        Some("pending")
    );
    assert_eq!(state.current_turn.text, "streaming");
    assert_eq!(
        crate::kit::atoms::INPUT_BUFFER
            .state()
            .read()
            .front()
            .map(|input| input.text.as_str()),
        Some("queued input")
    );
    assert!(
        crate::kit::atoms::HITL_PENDING.state().read().is_none(),
        "翻页不得发起审批"
    );
    assert!(
        crate::kit::atoms::POPUP_KIND.state().read().is_none(),
        "翻页不得打开弹窗"
    );
    assert!(state.todo_call_inputs.is_empty(), "翻页不得登记 Todo 调用");
    assert!(state.last_successful_todos.is_none());
    assert_eq!(state.current_request_id, None);
    // 历史内容仍然并入（隔离 ≠ 不显示）。
    assert!(unit_texts(&state).contains(&"old text".to_string()));
}

/// 跨页配对：终点（更晚一页先读到）与起点（更早一页后读到）最终合成一张终态卡片。
#[test]
#[serial]
fn test_history_page_pairs_tool_end_across_pages() {
    let mut state = browsing_state("tok");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);

    // 更晚的一页只含 tool_call_update（起点在更早一页，还没读到）。
    let newer = page(
        "tok",
        Some("cursor-2"),
        vec![(
            "s1:result",
            20,
            vec![sub(0, tool_end("t1", "file contents", false))],
        )],
    );
    dispatch_landing_page(&mut state, "tok", newer);
    // 更早的一页给出 tool_call 起点。
    let older = page(
        "tok",
        None,
        vec![("s1:call", 10, vec![sub(0, tool_start("t1", "Read"))])],
    );
    dispatch_landing_page(&mut state, "tok", older);

    let cards: Vec<&TuiRenderUnit> = state
        .committed
        .iter()
        .filter(|unit| matches!(unit, TuiRenderUnit::TuiToolCard(_)))
        .collect();
    assert_eq!(cards.len(), 1);
    let TuiRenderUnit::TuiToolCard(card) = cards[0] else {
        unreachable!()
    };
    assert_eq!(card.output_summary, "file contents");
    assert!(!card.is_error);
    assert!(!card.is_running);
}

/// P2：结果与起点跨页时，暂存必须保留**完整输出**（>512 KiB 也不能截成 1 KiB
/// 摘要），错误详情与尾部哨兵都必须出现在最终卡片上。
#[test]
#[serial]
fn test_history_page_cross_page_tool_result_keeps_full_output() {
    let mut state = browsing_state("tok");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
    crate::kit::acp_events::history_window::clear_pending_tool_ends();

    let tail = "TAIL-SENTINEL-END";
    let output = format!("{}{tail}", "x".repeat(600 * 1024));
    let newer = page(
        "tok",
        Some("cursor-2"),
        vec![(
            "s1:result",
            20,
            vec![sub(0, tool_end("t-big", &output, true))],
        )],
    );
    dispatch_landing_page(&mut state, "tok", newer);
    let older = page(
        "tok",
        None,
        vec![("s1:call", 10, vec![sub(0, tool_start("t-big", "Bash"))])],
    );
    dispatch_landing_page(&mut state, "tok", older);

    let card = state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiToolCard(card) if card.tool_id == "t-big" => Some(card),
            _ => None,
        })
        .expect("tool card");
    assert!(card.is_error);
    assert_eq!(
        card.output_summary.len(),
        output.len(),
        "pending result must keep the complete output"
    );
    assert!(
        card.output_summary.ends_with(tail),
        "tail (error detail / diff tail) must survive"
    );
    history_page::clear_pending_tool_ends_for_tests();
}

fn reminder_fixture() -> peri_acp_types::system_reminder::SystemReminder {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SYSTEM_REMINDER_VERSION,
    };
    peri_acp_types::system_reminder::SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Security,
        source: ReminderSource("permission".into()),
        kind: "mode_changed".into(),
        severity: ReminderSeverity::Warning,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![ReminderAudience::Tui]),
        body: "reminder body".into(),
        summary: Some("权限模式已变更".into()),
        metadata: json!({}),
    }
}

/// A20 极端形状：首屏自有条目的唯一展示单元是 reminder。它同样必须进区间索引，
/// 否则 `insert_slot_for` 兜底返回槽位 0，更早的历史会被插到继承前缀之前
/// （REQ-H01-6）。
#[test]
#[serial]
fn test_history_page_keeps_prefix_before_reminder_only_own_entry() {
    let mut state = browsing_state("tok");
    state.committed = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
        "inherited".to_string(),
    ))]);
    // 回放形状：条目标记 → 唯一单元是结构化 reminder（非 session/update 出口）。
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayEntryMarked {
            entry_id: "s1:own-reminder".to_string(),
            position: 100,
        },
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::SystemReminder {
            reminder: reminder_fixture(),
            replay: true,
            source_agent_id: None,
        },
    );
    let response = page(
        "tok",
        None,
        vec![("s1:old-1", 10, vec![sub(0, user_update("old one"))])],
    );
    dispatch_landing_page(&mut state, "tok", response);

    // 生产路径没有回放结束通知：区间由并入前的 `close_own_entry` 收口，
    // 因此索引断言放在并入之后（收口即是索引定型的时刻）。
    assert!(
        history_paging::with_spans(|spans| spans.contains("s1:own-reminder")),
        "首屏自有 reminder 条目必须登记区间：{}",
        paging_probe()
    );
    let texts = unit_texts(&state);
    assert_eq!(texts.len(), 3, "{texts:?} {}", paging_probe());
    assert_eq!(texts[0], "inherited", "更早历史不得越过继承前缀");
    assert_eq!(texts[1], "old one");
    assert!(
        matches!(state.committed[2], TuiRenderUnit::TuiSystemReminder(_)),
        "reminder 条目仍在自有区间内：{}",
        paging_probe()
    );
}

/// 同会话 rewind：旧范围的在途页不得并入按 `messages_json` 重建的窗口
/// （REQ-C01-5、A11）。rewind 时刻页 RPC 已认领、之后才返回——客户端侧
/// 会话与快照 token 都还对得上，唯一能兜住它的是 rewind 时本地失效浏览状态。
#[test]
#[serial]
fn test_history_page_is_dropped_after_same_session_rewind() {
    let mut state = browsing_state("tok");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
    // 触发侧已经认领了一次取页（页 RPC 在途）。
    crate::kit::atoms::HISTORY_PAGING.state().write().in_flight =
        Some(history_paging::InFlightPage {
            session_id: "s1".to_string(),
            snapshot_token: "tok".to_string(),
            cursor: "cursor-1".to_string(),
            limit: history_paging::PAGE_LIMIT,
        });

    // rewind 完成：committed 由回退后的消息 JSON 重建。
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::RewindCompleted {
            messages_json: json!([{"id": "m1", "role": "user", "content": "kept"}]).to_string(),
        },
    );
    let texts_after_rewind = unit_texts(&state);
    let epoch_after_rewind = crate::kit::atoms::HISTORY_PREPEND_EPOCH.get();

    // 在途页随后返回。
    let late = page(
        "tok",
        None,
        vec![("s1:old", 10, vec![sub(0, text_update("late page"))])],
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryPageLoaded {
            response: Box::new(late),
        },
    );

    assert_eq!(
        unit_texts(&state),
        texts_after_rewind,
        "rewind 后的迟到页不得并入：{}",
        paging_probe()
    );
    assert_eq!(
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.get(),
        epoch_after_rewind,
        "被丢弃的页不得推进锚点补偿代数"
    );
    let paging = crate::kit::atoms::HISTORY_PAGING.state().read().clone();
    assert!(
        paging.next_cursor.is_none()
            && paging.snapshot_token.is_none()
            && paging.in_flight.is_none(),
        "rewind 后浏览身份与在途认领必须失效：{paging:?}"
    );
}

/// A10：历史页「结果先到、起点未到」的暂存结果只能配对到**历史来源**的卡片。
/// 同 `tool_id` 的实时卡片（`TurnDone` 后落进 `committed`）不得被它改写输出位；
/// 旧实现按整个 `committed` 线性找 `tool_id`，会把历史结果写到实时卡片上。
#[test]
#[serial]
fn test_pending_history_tool_end_never_completes_live_card() {
    let mut state = browsing_state("tok");

    // 实时 turn：一个 tool_id = t1 的卡片，回合结束后留在窗口里。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".to_string(),
            tool_name: "Read".to_string(),
            input_summary: "live input".to_string(),
            raw_input: json!({"file_path": "/tmp/live"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let live_cards: Vec<&TuiRenderUnit> = state
        .committed
        .iter()
        .filter(|unit| matches!(unit, TuiRenderUnit::TuiToolCard(_)))
        .collect();
    assert_eq!(
        live_cards.len(),
        1,
        "实时卡片应已落进窗口：{}",
        paging_probe()
    );
    let TuiRenderUnit::TuiToolCard(live) = live_cards[0] else {
        unreachable!()
    };
    // 归档前 `flush_current_turn` 会先 `deactivate`（acp_events/mod.rs），因此落进
    // committed 的实时卡片恒为非 running——错配的危害表现在历史输出被写进它的
    // 输出位/错误位（REQ-H03、A10：历史不得挂到实时 turn）。
    assert!(!live.is_running);
    assert!(live.output_summary.is_empty(), "{}", paging_probe());
    assert!(!live.is_error);

    // 页 1：只有终点（起点在更早一页，还没读到）→ 进暂存表，本页无插入。
    let ends_only = page(
        "tok",
        Some("cursor-2"),
        vec![(
            "s1:result",
            20,
            vec![sub(0, tool_end("t1", "history output", false))],
        )],
    );
    dispatch_landing_page(&mut state, "tok", ends_only);
    // 页 2：可展示条目落地 → 触发暂存表结算。
    let older = page(
        "tok",
        None,
        vec![("s1:old", 10, vec![sub(0, user_update("old one"))])],
    );
    dispatch_landing_page(&mut state, "tok", older);

    let cards: Vec<&TuiRenderUnit> = state
        .committed
        .iter()
        .filter(|unit| matches!(unit, TuiRenderUnit::TuiToolCard(_)))
        .collect();
    let TuiRenderUnit::TuiToolCard(live) = cards[0] else {
        unreachable!()
    };
    assert!(
        live.output_summary.is_empty(),
        "实时卡片不得带上历史输出：{}",
        paging_probe()
    );
    assert!(
        !live.is_error,
        "实时卡片不得继承历史错误位：{}",
        paging_probe()
    );
    assert!(
        unit_texts(&state).contains(&"old one".to_string()),
        "同一页的历史内容仍必须并入：{}",
        paging_probe()
    );
    // 暂存表是进程级静态状态：本用例刻意留下未匹配终点，跑完必须清掉，
    // 否则同 `tool_id` 的后续用例会被它改写终态。
    history_page::clear_pending_tool_ends_for_tests();
}
