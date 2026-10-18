//! 超预算工具结果的延迟补读与展开认领（自 `history_page_test` 拆分）。

use super::*;

/// 超过内存预算的跨页结果保留来源页请求定位；用户展开后按该 cursor 补读完整正文。
#[tokio::test]
#[serial]
async fn test_history_page_over_budget_result_defers_then_refetches_by_origin_cursor() {
    use crate::acp_client::AcpTuiClient;
    use crate::kit::history_page_consumer::HistoryPageOrigin;
    use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};
    use std::time::Duration;

    let mut state = browsing_state("tok");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
    crate::kit::history_page_consumer::clear_page_origins_for_tests();
    history_page::clear_pending_tool_ends_for_tests();

    let tail = "TAIL-9MIB-END";
    let output = format!("{}{tail}", "x".repeat(9 * 1024 * 1024));
    let output_len = output.len();
    // 结果页的请求定位：补读必须回到这个 cursor，而不是响应 nextCursor。
    crate::kit::history_page_consumer::note_page_origin(HistoryPageOrigin {
        session_id: "s1".into(),
        snapshot_token: "tok".into(),
        cursor: "cursor-result".into(),
        limit: 50,
    });
    dispatch_landing_page(
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

    let card = state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiToolCard(card) if card.tool_id == "t-big" => Some(card),
            _ => None,
        })
        .expect("tool card");
    assert_eq!(
        card.output_summary, "结果正文较大，展开后按需加载",
        "over-budget result must not be replaced by a 1 KiB summary"
    );
    let source =
        crate::kit::acp_events::history_window::deferred_tool_result_source_for_tests("t-big")
            .expect("deferred source");
    assert_eq!(source.cursor, "cursor-result");
    assert_eq!(source.limit, 50);
    assert_eq!(source.session_id, "s1");
    assert_eq!(source.snapshot_token, "tok");
    assert_eq!(source.entry_id, "s1:result");
    assert_eq!(source.sub_item_position, 0);

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

    let response = history_page::refetch_tool_result(&client, &source)
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

    history_page::note_force_complete_tool_for_tests("t-big");
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryPageLoaded {
            response: Box::new(response),
        },
    );
    let card = state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiToolCard(card) if card.tool_id == "t-big" => Some(card),
            _ => None,
        })
        .expect("tool card");
    assert_eq!(
        card.output_summary.len(),
        output_len,
        "refetched output must be complete"
    );
    assert!(
        card.output_summary.ends_with(tail),
        "tail must survive the on-demand refetch"
    );
    history_page::clear_pending_tool_ends_for_tests();
    crate::kit::history_page_consumer::clear_page_origins_for_tests();
}

/// 补读失败必须显式报错，不能回退成摘要或静默空白。
#[tokio::test]
#[serial]
async fn test_history_page_over_budget_result_refetch_failure_is_explicit() {
    use crate::acp_client::AcpTuiClient;
    use crate::kit::history_page_consumer::HistoryPageOrigin;
    use peri_acp::transport::{
        AcpTransport,
        mpsc::mpsc_transport_pair,
        types::{AcpError, IncomingMessage},
    };
    use std::time::Duration;

    let mut state = browsing_state("tok");
    seed_first_screen_own_entry(&mut state, "s1:own-latest", 100);
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
    let source = history_page::ToolResultSource {
        session_id: "s1".into(),
        snapshot_token: "tok".into(),
        cursor: "cursor-result".into(),
        limit: 50,
        entry_id: "s1:result".into(),
        entry_position: 20,
        sub_item_position: 0,
        tool_id: "t-big".into(),
    };
    crate::kit::acp_events::history_window::insert_deferred_tool_result_for_tests(
        "t-big",
        source.clone(),
    );
    history_paging::with_spans(|spans| history_page::apply_pending_tool_ends(&mut state, spans));

    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, _tx, _rx) = AcpTuiClient::new(client_transport);
    let server = tokio::spawn(async move {
        let message = tokio::time::timeout(Duration::from_secs(2), server_transport.recv())
            .await
            .expect("refetch request")
            .expect("server open");
        let IncomingMessage::Request { id, .. } = message else {
            panic!("expected request");
        };
        server_transport
            .send_response(id, Err(AcpError::new(-32012, "range invalid")))
            .await
            .expect("respond");
    });
    let error = history_page::refetch_tool_result(&client, &source)
        .await
        .expect_err("refetch must fail");
    server.await.expect("server task");
    assert!(
        error.contains("历史结果补读失败"),
        "explicit error expected, got {error}"
    );

    let error_response =
        crate::kit::acp_events::history_window::tool_result_error_response_for_tests(
            &source, &error,
        );
    dispatch_for_bridge(
        &mut state,
        &AcpEventData::HistoryPageLoaded {
            response: Box::new(error_response),
        },
    );
    let card = state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiToolCard(card) if card.tool_id == "t-big" => Some(card),
            _ => None,
        })
        .expect("tool card");
    assert!(
        card.output_summary.contains("历史结果补读失败"),
        "failure must be visible on the card"
    );
    assert!(card.is_error);
    let _ = HistoryPageOrigin {
        session_id: "s1".into(),
        snapshot_token: "tok".into(),
        cursor: "cursor-result".into(),
        limit: 50,
    };
    history_page::clear_pending_tool_ends_for_tests();
    crate::kit::history_page_consumer::clear_page_origins_for_tests();
}

/// 展开工具卡片会认领 deferred 来源定位（进入在途，避免重复补读）。
#[test]
#[serial]
fn test_expanded_tool_card_claims_deferred_result_refetch() {
    use crate::kit::tui_render_unit::{FoldState, TuiToolCard};

    crate::kit::atoms::init_atoms();
    crate::kit::atoms::NOTIFICATION.set(None);
    history_page::clear_pending_tool_ends_for_tests();
    let source = history_page::ToolResultSource {
        session_id: "s1".into(),
        snapshot_token: "tok".into(),
        cursor: "cursor-result".into(),
        limit: 50,
        entry_id: "s1:result".into(),
        entry_position: 20,
        sub_item_position: 0,
        tool_id: "t-big".into(),
    };
    crate::kit::acp_events::history_window::insert_deferred_tool_result_for_tests(
        "t-big",
        source.clone(),
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
