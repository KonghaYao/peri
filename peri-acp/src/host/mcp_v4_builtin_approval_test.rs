use super::*;

// ── 契约 6 BLOCKED 缺口复证（A10/R19）：提升 + 审批 + wire ────────────────────

/// BLOCKED 缺口复证（approve）：走**真实配置路径**提升为 direct 的用户 System MCP
/// 工具被模型调用时，审批恰好发生一次，**线路上恰好一条 `tools/call`**，且 wire 上的
/// 工具名是原始名（归一只作用于判定/匹配，不得泄漏到 wire —— IF-D15 / §9 规则 11）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn promoted_direct_tool_approval_calls_wire_exactly_once() {
    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    let broker = Arc::new(RecordingBroker::new(BrokerDecision::Approve));
    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![ScriptedToolCall::new(
        WIRE_FIXTURE_ECHO_EFFECTIVE_NAME,
        serde_json::json!({ "query": "v02-host-approve" }),
    )]));
    let result = run_wire_prompt(
        session_context_with_broker(&harness, "mcp-v4-builtin-approve", Arc::clone(&broker)).await,
        &sink,
        &model,
    )
    .await;

    assert!(result.ok, "批准路径必须正常收束: {:?}", result.failure);
    assert_eq!(
        broker.requests(),
        1,
        "被提升为 direct 的工具必须恰好触发一次审批"
    );
    assert_eq!(
        broker
            .seen()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        vec![WIRE_FIXTURE_ECHO_EFFECTIVE_NAME.to_string()],
        "审批 broker 收到的必须是模型面 effective name"
    );

    let calls = wire_tool_calls(&harness);
    assert_eq!(
        calls.len(),
        1,
        "批准后 wire 上恰好一条 tools/call: {calls:?}"
    );
    assert_eq!(
        calls[0]["params"]["name"], "echo",
        "wire 上必须是**原始工具名**（effective name 不得泄漏到 wire）: {calls:?}"
    );
    assert_eq!(
        calls[0]["params"]["arguments"],
        serde_json::json!({ "query": "v02-host-approve" }),
        "server 侧必须收到模型给出的入参（参数真的过了 wire）: {calls:?}"
    );

    let tool_ends = tool_end_events(&sink);
    assert_eq!(tool_ends.len(), 1, "工具结果必须唯一: {tool_ends:?}");
    assert_eq!(
        tool_ends[0].0, WIRE_FIXTURE_ECHO_EFFECTIVE_NAME,
        "事件载荷仍为 effective name（归一不污染投影真值）"
    );
    assert!(!tool_ends[0].2, "批准后的调用不得是错误结果");
    assert!(
        tool_ends[0].1.contains("wire_fixture:echo:ok"),
        "工具结果内容必须可辨认: {:?}",
        tool_ends[0].1
    );
}

/// BLOCKED 缺口复证（reject）：拒绝后链路**不得**触碰 wire（`tools/call` 计数为 0），
/// 结果为拒绝语义且只审批一次（不重试）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn promoted_direct_tool_rejection_never_reaches_wire() {
    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    let broker = Arc::new(RecordingBroker::new(BrokerDecision::Reject));
    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![ScriptedToolCall::new(
        WIRE_FIXTURE_ECHO_EFFECTIVE_NAME,
        serde_json::json!({ "query": "v02-host-reject" }),
    )]));
    let result = run_wire_prompt(
        session_context_with_broker(&harness, "mcp-v4-builtin-reject", Arc::clone(&broker)).await,
        &sink,
        &model,
    )
    .await;

    assert!(
        result.ok,
        "用户拒绝是正常终态（模型可见的 error 结果），不是 fatal: {:?}",
        result.failure
    );
    assert_eq!(broker.requests(), 1, "拒绝路径同样只审批一次，不得重试");

    let calls = wire_tool_calls(&harness);
    assert!(
        calls.is_empty(),
        "拒绝后 wire 上不得出现任何 tools/call: {calls:?}"
    );

    let tool_ends = tool_end_events(&sink);
    assert_eq!(
        tool_ends.len(),
        1,
        "拒绝必须产生一条模型可见的工具结果: {tool_ends:?}"
    );
    assert!(tool_ends[0].2, "拒绝必须是 error 结果: {tool_ends:?}");
    assert!(
        tool_ends[0].1.contains("v02 host seam reject"),
        "结果文案必须表达用户拒绝语义: {:?}",
        tool_ends[0].1
    );
}
