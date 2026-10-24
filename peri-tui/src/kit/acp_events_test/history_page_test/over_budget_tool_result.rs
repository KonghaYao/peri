//! 超预算工具结果的延迟补读与展开认领（自 `history_page_test` 拆分）。

use super::*;
use crate::kit::acp_events::history_page::{
    DEFERRED_TOOL_RESULT_TEXT, UNAVAILABLE_TOOL_RESULT_TEXT,
};
use crate::kit::acp_types::ToolResultRefetch;
use crate::kit::history_paging::HistoryPageOrigin;
use crate::kit::history_window::PersistentEntryKey;

fn source(tool_id: &str, token: &str, cursor: &str) -> history_page::ToolResultSource {
    history_page::ToolResultSource {
        session_id: "s1".into(),
        snapshot_token: token.into(),
        cursor: cursor.into(),
        limit: 50,
        entry_id: format!("s1:result-{tool_id}"),
        entry_position: 20,
        sub_item_position: 0,
        tool_id: tool_id.into(),
    }
}

fn origin(cursor: &str, limit: usize) -> HistoryPageOrigin {
    HistoryPageOrigin {
        session_id: "s1".into(),
        snapshot_token: "tok".into(),
        cursor: cursor.into(),
        limit,
    }
}

/// 派发一次补读响应（成功或显式失败），身份随事件传递。
fn dispatch_refetch(
    state: &mut BridgeState,
    response: HistoryPageResponse,
    request_id: u64,
    tool_id: &str,
    error: Option<String>,
) {
    dispatch_for_bridge(
        state,
        &AcpEventData::HistoryPageLoaded {
            response: Box::new(response),
            origin: None,
            refetch: Some(Box::new(ToolResultRefetch {
                request_id,
                tool_id: tool_id.to_string(),
                error,
            })),
        },
    );
}

fn card_output(state: &BridgeState, tool_id: &str) -> (String, bool) {
    state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiToolCard(card) if card.tool_id == tool_id => {
                Some((card.output_summary.clone(), card.is_error))
            }
            _ => None,
        })
        .expect("tool card")
}

/// 超过内存预算的跨页结果保留来源页请求定位；用户展开后按该 cursor 补读完整正文。
#[tokio::test]
#[serial]
async fn test_live_tool_card_with_persistent_position_defers_result_with_source() {
    use crate::kit::history_window::{
        HistoryWindow, HistoryWindowConfig, PersistentEntryKey, WindowIdentity,
    };

    let mut state = browsing_state("tok");
    state.window = HistoryWindow::new(HistoryWindowConfig::enabled());
    state
        .window
        .begin_window(WindowIdentity::new("s1", "gen-1", 7, "snap-1"));
    history_page::clear_pending_tool_ends_for_tests();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "live answer".into(),
            message_id: Some("m-live".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: "t-live".into(),
            tool_name: "Bash".into(),
            input_summary: "run".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    // 页回执给同一持久条目的位置；live 卡随条目进入可补读区间。
    let key = PersistentEntryKey::own("s1:m-live");
    assert!(state.window.promote_live_seal_position(&key, 10));
    crate::kit::history_paging::sync_spans_from_window(&state.window);

    let tail_text = "TAIL-LIVE-END";
    let output = format!("{}{tail_text}", "x".repeat(9 * 1024 * 1024));
    dispatch_landing_page_with_origin(
        &mut state,
        "tok",
        page(
            "tok",
            Some("cursor-2"),
            vec![(
                "s1:result",
                20,
                vec![sub(0, tool_end("t-live", &output, false))],
            )],
        ),
        Some(origin("cursor-result", 50)),
    );

    let (summary, is_error) = card_output(&state, "t-live");
    assert_eq!(
        summary, DEFERRED_TOOL_RESULT_TEXT,
        "live card with a persistent position must take the deferred placeholder"
    );
    assert!(!is_error);
    let deferred_source =
        crate::kit::acp_events::history_window::deferred_tool_result_source_for_tests("t-live")
            .expect("live card keeps the result page source");
    assert_eq!(deferred_source.cursor, "cursor-result");
    assert_eq!(deferred_source.limit, 50);
    assert_eq!(deferred_source.session_id, "s1");
    assert_eq!(deferred_source.snapshot_token, "tok");
    assert_eq!(deferred_source.entry_id, "s1:result");
    history_page::clear_pending_tool_ends_for_tests();
}

#[tokio::test]
#[serial]
async fn test_history_page_over_budget_result_defers_then_refetches_by_origin_cursor() {
    use crate::acp_client::AcpTuiClient;
    use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};
    use std::time::Duration;

    let mut state = browsing_state("tok");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
    history_page::clear_pending_tool_ends_for_tests();

    let tail = "TAIL-9MIB-END";
    let output = format!("{}{tail}", "x".repeat(9 * 1024 * 1024));
    let output_len = output.len();
    // 结果页的请求定位随事件传递：补读必须回到这个 cursor，而不是响应 nextCursor。
    dispatch_landing_page_with_origin(
        &mut state,
        "tok",
        page(
            "tok",
            Some("cursor-2"),
            vec![(
                "s1:result",
                20,
                vec![sub(0, tool_end("t-big", &output, false))],
            )],
        ),
        Some(origin("cursor-result", 50)),
    );
    dispatch_landing_page(
        &mut state,
        "tok",
        page(
            "tok",
            None,
            vec![("s1:call", 10, vec![sub(0, tool_start("t-big", "Bash"))])],
        ),
    );

    let (deferred, is_error) = card_output(&state, "t-big");
    assert_eq!(
        deferred, DEFERRED_TOOL_RESULT_TEXT,
        "over-budget result must not be replaced by a 1 KiB summary"
    );
    assert!(!is_error);
    let deferred_source =
        crate::kit::acp_events::history_window::deferred_tool_result_source_for_tests("t-big")
            .expect("deferred source");
    assert_eq!(deferred_source.cursor, "cursor-result");
    assert_eq!(deferred_source.limit, 50);
    assert_eq!(deferred_source.session_id, "s1");
    assert_eq!(deferred_source.snapshot_token, "tok");
    assert_eq!(deferred_source.entry_id, "s1:result");
    assert_eq!(deferred_source.sub_item_position, 0);

    // 用户展开：认领来源并进入在途。
    let claimed =
        crate::kit::acp_events::history_window::claim_deferred_tool_result_for_tests("t-big", 7)
            .expect("claim deferred result");
    assert_eq!(claimed.cursor, "cursor-result");

    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, _tx, _rx) = AcpTuiClient::new(client_transport);
    let server_output = output;
    let server = tokio::spawn(async move {
        let message = tokio::time::timeout(Duration::from_secs(2), server_transport.recv())
            .await
            .expect("refetch request")
            .expect("server open");
        let IncomingMessage::Request { id, method, params } = message else {
            panic!("expected request");
        };
        assert_eq!(method, "session/load");
        assert_eq!(params["sessionId"], "s1");
        assert_eq!(params["cursor"], "cursor-result");
        assert_eq!(params["limit"], 50);
        server_transport
            .send_response(
                id,
                Ok(json!({
                    "sessionId": "s1",
                    "snapshotToken": "tok",
                    "nextCursor": null,
                    "items": [{
                        "entryId": "s1:result",
                        "position": 20,
                        "subItems": [{
                            "position": 0,
                            "update": {
                                "sessionUpdate": "tool_call_update",
                                "toolCallId": "t-big",
                                "rawOutput": server_output,
                                "status": "completed",
                            },
                        }],
                    }],
                })),
            )
            .await
            .expect("respond");
    });

    let response = history_page::refetch_tool_result(&client, &deferred_source)
        .await
        .expect("refetch full result");
    server.await.expect("server task");
    assert_eq!(
        response.items[0]
            .sub_items
            .first()
            .and_then(|sub| sub.update.get("rawOutput"))
            .and_then(serde_json::Value::as_str)
            .map(str::len),
        Some(output_len)
    );

    dispatch_refetch(&mut state, response, 7, "t-big", None);
    let (refetched, refetched_is_error) = card_output(&state, "t-big");
    assert_eq!(
        refetched.len(),
        output_len,
        "refetched output must be complete"
    );
    assert!(
        refetched.ends_with(tail),
        "tail must survive the on-demand refetch"
    );
    assert!(!refetched_is_error);
    assert_eq!(
        crate::kit::acp_events::history_window::deferred_in_flight_request_for_tests("t-big"),
        None,
        "成功补读必须释放在途认领"
    );
    history_page::clear_pending_tool_ends_for_tests();
}

/// 失败的工具结果：超预算占位仍必须显示失败样式（is_error 透传），
/// 正文是否驻留不能改变工具成功/失败语义。
#[test]
#[serial]
fn test_history_page_over_budget_error_result_keeps_error_placeholder() {
    let mut state = browsing_state("tok");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
    history_page::clear_pending_tool_ends_for_tests();

    let output = format!("{}{tail}", "x".repeat(9 * 1024 * 1024), tail = "FAILED");
    dispatch_landing_page_with_origin(
        &mut state,
        "tok",
        page(
            "tok",
            Some("cursor-2"),
            vec![(
                "s1:result",
                20,
                vec![sub(0, tool_end("t-big", &output, true))],
            )],
        ),
        Some(origin("cursor-result", 50)),
    );
    dispatch_landing_page(
        &mut state,
        "tok",
        page(
            "tok",
            None,
            vec![("s1:call", 10, vec![sub(0, tool_start("t-big", "Bash"))])],
        ),
    );

    let (summary, is_error) = card_output(&state, "t-big");
    assert_eq!(summary, DEFERRED_TOOL_RESULT_TEXT);
    assert!(
        is_error,
        "over-budget placeholder must preserve the tool's failed status"
    );
    history_page::clear_pending_tool_ends_for_tests();
}

/// 无来源定位的失败结果同样保留 is_error：显示「无法按需加载」不等于成功。
#[test]
#[serial]
fn test_history_page_over_budget_no_source_error_result_keeps_error_placeholder() {
    let mut state = browsing_state("tok");
    history_page::clear_pending_tool_ends_for_tests();
    dispatch_landing_page(
        &mut state,
        "tok",
        page(
            "tok",
            None,
            vec![("s1:call", 10, vec![sub(0, tool_start("t-big", "Bash"))])],
        ),
    );
    history_page::insert_pending_tool_end_for_tests(
        "t-big",
        PersistentEntryKey::own("s1:end-t-big"),
        &"x".repeat(9 * 1024 * 1024),
        true,
    );
    history_paging::with_spans(|spans| {
        history_page::apply_pending_tool_ends(&mut state, spans);
    });

    let (summary, is_error) = card_output(&state, "t-big");
    assert_eq!(summary, UNAVAILABLE_TOOL_RESULT_TEXT);
    assert!(is_error, "无来源占位也必须显示失败样式");
    history_page::clear_pending_tool_ends_for_tests();
}

/// 两路并发补读：响应 A 先到、响应 B 后到，各自只补全自己的 tool_id，
/// 不允许单槽标记互相覆盖（旧实现的确定性失败）。
#[test]
#[serial]
fn test_interleaved_refetch_results_do_not_steal_force_complete() {
    let mut state = browsing_state("tok");
    history_page::clear_pending_tool_ends_for_tests();
    dispatch_landing_page(
        &mut state,
        "tok",
        page(
            "tok",
            None,
            vec![(
                "s1:calls",
                10,
                vec![
                    sub(0, tool_start("tA", "Bash")),
                    sub(1, tool_start("tB", "Bash")),
                ],
            )],
        ),
    );
    crate::kit::acp_events::history_window::insert_deferred_tool_result_for_tests(
        "tA",
        source("tA", "tok", "cursor-a"),
    );
    crate::kit::acp_events::history_window::insert_deferred_tool_result_for_tests(
        "tB",
        source("tB", "tok", "cursor-b"),
    );
    crate::kit::acp_events::history_window::claim_deferred_tool_result_for_tests("tA", 1)
        .expect("claim tA");
    crate::kit::acp_events::history_window::claim_deferred_tool_result_for_tests("tB", 2)
        .expect("claim tB");

    let tail_a = "A-END";
    let tail_b = "B-END";
    let output_a = format!("{}{tail_a}", "a".repeat(9 * 1024 * 1024));
    let output_b = format!("{}{tail_b}", "b".repeat(9 * 1024 * 1024));
    let response_a = page(
        "tok",
        None,
        vec![(
            "s1:result-tA",
            21,
            vec![sub(0, tool_end("tA", &output_a, false))],
        )],
    );
    let response_b = page(
        "tok",
        None,
        vec![(
            "s1:result-tB",
            22,
            vec![sub(0, tool_end("tB", &output_b, false))],
        )],
    );

    // 响应顺序与认领顺序相反，仍必须各归各的 tool_id。
    dispatch_refetch(&mut state, response_a, 1, "tA", None);
    dispatch_refetch(&mut state, response_b, 2, "tB", None);

    let (summary_a, error_a) = card_output(&state, "tA");
    let (summary_b, error_b) = card_output(&state, "tB");
    assert_eq!(summary_a.len(), output_a.len(), "响应 A 必须完整落地");
    assert!(summary_a.ends_with(tail_a), "A 的尾部不得丢失");
    assert!(!error_a);
    assert_eq!(summary_b.len(), output_b.len(), "响应 B 必须完整落地");
    assert!(summary_b.ends_with(tail_b), "B 的尾部不得丢失");
    assert!(!error_b);
    assert_eq!(
        crate::kit::acp_events::history_window::pending_tool_ends_fingerprint(),
        (0, 0),
        "两路补读完成后暂存表必须清空"
    );
    history_page::clear_pending_tool_ends_for_tests();
}

/// 补读标记只作用于目标 tool_id：tA 的补读完成后，普通页里的 tB 超预算结果
/// 不得因残留标记获得一次性完整正文。
#[test]
#[serial]
fn test_refetch_force_marker_is_scoped_to_its_tool() {
    let mut state = browsing_state("tok");
    history_page::clear_pending_tool_ends_for_tests();
    dispatch_landing_page(
        &mut state,
        "tok",
        page(
            "tok",
            None,
            vec![(
                "s1:calls",
                10,
                vec![
                    sub(0, tool_start("tA", "Bash")),
                    sub(1, tool_start("tB", "Bash")),
                ],
            )],
        ),
    );
    crate::kit::acp_events::history_window::insert_deferred_tool_result_for_tests(
        "tA",
        source("tA", "tok", "cursor-a"),
    );
    crate::kit::acp_events::history_window::claim_deferred_tool_result_for_tests("tA", 3)
        .expect("claim tA");

    let output_a = format!("{}A-END", "a".repeat(9 * 1024 * 1024));
    dispatch_refetch(
        &mut state,
        page(
            "tok",
            None,
            vec![(
                "s1:result-tA",
                21,
                vec![sub(0, tool_end("tA", &output_a, false))],
            )],
        ),
        3,
        "tA",
        None,
    );
    let (summary_a, _) = card_output(&state, "tA");
    assert_eq!(summary_a.len(), output_a.len(), "tA 必须补读完整");

    let output_b = format!("{}B-END", "b".repeat(9 * 1024 * 1024));
    dispatch_landing_page(
        &mut state,
        "tok",
        page(
            "tok",
            None,
            vec![(
                "s1:result-tB",
                22,
                vec![sub(0, tool_end("tB", &output_b, false))],
            )],
        ),
    );
    let (summary_b, _) = card_output(&state, "tB");
    assert_eq!(
        summary_b, UNAVAILABLE_TOOL_RESULT_TEXT,
        "普通页的 tB 不得从 tA 的补读标记获得超预算正文"
    );
    history_page::clear_pending_tool_ends_for_tests();
}

/// 来源随响应事件传递：逆序派发两页，各自的 deferred source 仍指向自己的
/// 请求 cursor（旧实现按全局队列取「最新」一条，会串页）。
#[test]
#[serial]
fn test_page_origin_follows_its_response_event() {
    let mut state = browsing_state("tok");
    history_page::clear_pending_tool_ends_for_tests();

    let output_a = format!("{}A-END", "a".repeat(9 * 1024 * 1024));
    let output_b = format!("{}B-END", "b".repeat(9 * 1024 * 1024));
    dispatch_landing_page_with_origin(
        &mut state,
        "tok",
        page(
            "tok",
            None,
            vec![(
                "s1:result-tA",
                21,
                vec![sub(0, tool_end("tA", &output_a, false))],
            )],
        ),
        Some(origin("cursor-A", 50)),
    );
    dispatch_landing_page_with_origin(
        &mut state,
        "tok",
        page(
            "tok",
            None,
            vec![(
                "s1:result-tB",
                22,
                vec![sub(0, tool_end("tB", &output_b, false))],
            )],
        ),
        Some(origin("cursor-B", 60)),
    );

    let source_a =
        crate::kit::acp_events::history_window::deferred_tool_result_source_for_tests("tA")
            .expect("tA source");
    let source_b =
        crate::kit::acp_events::history_window::deferred_tool_result_source_for_tests("tB")
            .expect("tB source");
    assert_eq!(source_a.cursor, "cursor-A");
    assert_eq!(source_a.limit, 50);
    assert_eq!(source_b.cursor, "cursor-B");
    assert_eq!(source_b.limit, 60);
    history_page::clear_pending_tool_ends_for_tests();
}

/// 补读响应被浏览身份过滤丢弃时也必须释放在途认领，用户可以再次展开重试。
#[test]
#[serial]
fn test_refetch_response_releases_in_flight_when_page_dropped() {
    use crate::kit::acp_events::history_window;

    let mut state = browsing_state("tok-current");
    history_page::clear_pending_tool_ends_for_tests();
    history_window::insert_deferred_tool_result_for_tests(
        "t-big",
        source("t-big", "tok-current", "cursor-result"),
    );
    assert!(
        history_window::claim_deferred_tool_result_for_tests("t-big", 7).is_some(),
        "首次展开必须认领"
    );
    assert_eq!(
        history_window::deferred_in_flight_request_for_tests("t-big"),
        Some(7)
    );

    // 快照已经变化：整页丢弃，但请求必须结束。
    let stale = page(
        "tok-stale",
        None,
        vec![(
            "s1:result-t-big",
            20,
            vec![sub(0, tool_end("t-big", "late", false))],
        )],
    );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryPageLoaded {
            response: Box::new(stale),
            origin: None,
            refetch: Some(Box::new(ToolResultRefetch {
                request_id: 7,
                tool_id: "t-big".into(),
                error: None,
            })),
        },
    );

    assert_eq!(
        history_window::deferred_in_flight_request_for_tests("t-big"),
        None,
        "丢弃的响应不得让 in_flight 永久占用"
    );
    assert!(
        history_window::claim_deferred_tool_result_for_tests("t-big", 8).is_some(),
        "释放在途后必须允许重新补读"
    );
    history_page::clear_pending_tool_ends_for_tests();
}

/// 服务端静默（连接未断、永不响应）：补读 RPC 必须在有限预算内失败，
/// 且原因显式可展示（不是摘要或空白）。
#[tokio::test]
#[serial]
async fn test_refetch_times_out_with_explicit_error() {
    use crate::acp_client::AcpTuiClient;
    use peri_acp::transport::mpsc::mpsc_transport_pair;
    use std::time::{Duration, Instant};

    let _state = browsing_state("tok");
    let deferred_source = source("t-big", "tok", "cursor-result");
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, _tx, _rx) = AcpTuiClient::new(client_transport);

    let started = Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        history_page::refetch_tool_result_with_budget(
            &client,
            &deferred_source,
            Duration::from_millis(50),
        ),
    )
    .await
    .expect("补读必须有有限预算，静默服务端不得让任务悬挂")
    .expect_err("静默服务端不得让补读成功");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "超时必须及时返回，实际 {:?}",
        started.elapsed()
    );
    assert!(
        error.contains("历史结果补读失败") && error.contains("超时"),
        "超时原因必须显式：{error}"
    );
    // 连接保持存活：关闭会以传输错误结算，覆盖不到「连接沉默」这条路径。
    let _ = server_transport;
}

/// 补读预算契约：沿用普通翻页的 10s 期限。
#[test]
#[serial]
fn test_refetch_timeout_budget_is_ten_seconds() {
    assert_eq!(
        crate::kit::acp_events::history_window::refetch_timeout_for_tests(),
        std::time::Duration::from_secs(10)
    );
}

/// 静默超时后的完整闭环：失败显式呈现、释放在途、再次展开可重试并成功。
#[tokio::test]
#[serial]
async fn test_refetch_timeout_releases_in_flight_and_allows_retry() {
    use crate::acp_client::AcpTuiClient;
    use crate::kit::acp_events::history_window;
    use peri_acp::transport::mpsc::mpsc_transport_pair;
    use std::time::Duration;

    let mut state = browsing_state("tok");
    history_page::clear_pending_tool_ends_for_tests();
    dispatch_landing_page(
        &mut state,
        "tok",
        page(
            "tok",
            None,
            vec![("s1:call", 10, vec![sub(0, tool_start("t-big", "Bash"))])],
        ),
    );
    history_window::insert_deferred_tool_result_for_tests(
        "t-big",
        source("t-big", "tok", "cursor-result"),
    );
    assert!(
        history_window::claim_deferred_tool_result_for_tests("t-big", 21).is_some(),
        "展开必须先认领"
    );

    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, _tx, _rx) = AcpTuiClient::new(client_transport);
    let error = history_page::refetch_tool_result_with_budget(
        &client,
        &source("t-big", "tok", "cursor-result"),
        Duration::from_millis(50),
    )
    .await
    .expect_err("静默服务端必须超时");
    let error_response =
        crate::kit::acp_events::history_window::tool_result_error_response_for_tests(
            &source("t-big", "tok", "cursor-result"),
            &error,
        );
    dispatch_refetch(&mut state, error_response, 21, "t-big", Some(error));

    let (failure_text, is_error) = card_output(&state, "t-big");
    assert!(
        failure_text.contains("历史结果补读失败"),
        "超时必须显示在卡片上：{failure_text}"
    );
    assert!(is_error, "补读失败必须显示失败样式");
    assert_eq!(
        history_window::deferred_in_flight_request_for_tests("t-big"),
        None,
        "失败/超时后必须释放在途认领"
    );
    assert!(
        history_window::claim_deferred_tool_result_for_tests("t-big", 22).is_some(),
        "释放在途后用户再次展开必须能重试"
    );

    let tail = "RETRY-END";
    let output = format!("{}{tail}", "r".repeat(9 * 1024 * 1024));
    dispatch_refetch(
        &mut state,
        page(
            "tok",
            None,
            vec![(
                "s1:result-t-big",
                20,
                vec![sub(0, tool_end("t-big", &output, false))],
            )],
        ),
        22,
        "t-big",
        None,
    );
    let (retried, retried_is_error) = card_output(&state, "t-big");
    assert_eq!(retried.len(), output.len(), "重试必须补全完整正文");
    assert!(retried.ends_with(tail));
    assert!(!retried_is_error, "重试成功后必须恢复成功样式");
    history_page::clear_pending_tool_ends_for_tests();
    let _ = server_transport;
}

/// 展开工具卡片会认领 deferred 来源定位（进入在途，避免重复补读）。
#[test]
#[serial]
fn test_expanded_tool_card_claims_deferred_result_refetch() {
    use crate::kit::tui_render_unit::{FoldState, TuiToolCard};

    crate::kit::atoms::init_atoms();
    crate::kit::atoms::NOTIFICATION.set(None);
    history_page::clear_pending_tool_ends_for_tests();
    crate::kit::acp_events::history_window::insert_deferred_tool_result_for_tests(
        "t-big",
        source("t-big", "tok", "cursor-result"),
    );
    let card = TuiToolCard {
        tool_id: "t-big".into(),
        tool_name: "Bash".into(),
        input_summary: String::new(),
        output_summary: "结果正文较大，展开后按需加载".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: None,
        diff: None,
        presentation: Default::default(),
        fold: FoldState::Expanded,
        user_modified: true,
        tool_calls_count: 0,
        content_hash: 0,
    };
    let claimed = crate::kit::acp_events::history_window::maybe_request_tool_result(
        &TuiRenderUnit::TuiToolCard(card),
    )
    .expect("expanded card must claim the deferred source");
    assert_eq!(claimed.cursor, "cursor-result");
    assert!(
        crate::kit::atoms::NOTIFICATION.state().read().is_some(),
        "no client/runtime in this unit test must surface an explicit failure"
    );
    history_page::clear_pending_tool_ends_for_tests();
}
