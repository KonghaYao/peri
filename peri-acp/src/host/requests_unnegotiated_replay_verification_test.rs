use super::*;

// ── S9-verification：独立复核用例（复核者加入，非实现者用例）─────────────────────

/// 未协商连接的初始 load 回放**逐条等于** `load_session_history` 的 canonical 输出。
///
/// 与实现者用例的差别（独立复核意图）：
/// - 载荷类型混合：human / AI blocks（reasoning + text + tool_use）/ AI tool_calls 与正文
///   并存 / tool 结果 / System；
/// - 归档行**交错**在中间（不是前缀），且被归档的是工具调用与工具结果；
/// - 期望序列在本用例内按 ACP 投影契约**独立重算**（不调用 `project_payload_updates`），
///   逐条比较 `messageId` / 正文 / 工具调用身份与顺序。
#[tokio::test]
async fn s9_verification_unnegotiated_replay_equals_canonical_history_for_mixed_payloads() {
    let mut fixture = ReplayFixture::new(SpyMode::Forward).await;
    fixture.create_session("s9-verification-mixed").await;
    fixture
        .append(&[
            BaseMessage::human("归档一：问题"),
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::reasoning("归档一：思考"),
                ContentBlock::text("归档一：回答"),
                ContentBlock::tool_use("call-arch", "bash", json!({ "cmd": "ls" })),
            ]),
            BaseMessage::tool_result("call-arch", "归档一：工具结果"),
            BaseMessage::human("有效：第二个问题"),
            BaseMessage::ai_with_tool_calls(
                "有效：调用工具",
                vec![ToolCallRequest::new(
                    "call-live",
                    "read_file",
                    json!({ "path": "a.rs" }),
                )],
            ),
            BaseMessage::tool_result("call-live", "有效：工具结果"),
            BaseMessage::system("系统提示：不计入展示"),
            BaseMessage::ai("有效：最后回答"),
        ])
        .await;
    let canonical = fixture.canonical().await;
    assert_eq!(canonical.len(), 8, "夹具前提：8 条持久化载荷");
    // 交错的归档段（工具调用 + 工具结果 + 一条 human），不是前缀。
    fixture.archive(&payload_ids(&canonical)[1..4]).await;
    fixture.set_caps(PeriCaps::default());

    let effective = fixture.effective().await;
    assert_eq!(
        effective.len(),
        5,
        "夹具前提：归档后有效集合只剩 m0 + m4..m7（回放不得以它为来源）"
    );
    assert_eq!(
        canonical
            .iter()
            .filter(|p| payload_text(p).starts_with("归档"))
            .count(),
        3,
        "夹具前提：canonical 读取含三条归档原文"
    );
    fixture.reset_reads();

    let response = fixture.load().await.unwrap();

    let observed = observed_wire_facts(&fixture);
    let expected = expected_wire_facts(&canonical);
    assert_eq!(
        observed, expected,
        "未协商连接的回放序列必须逐条等于 canonical 输出（身份 / 正文 / 工具身份 / 顺序）"
    );
    assert_eq!(fixture.spy.full_reads(), 0, "流式路径不读全量");

    // 无私有语义：响应与全部通知里不得出现分页 / 代次私有字段（默认 caps 连接）。
    let response_keys = all_keys(&response);
    let notification_keys = replay_keys(&fixture);
    let private: Vec<&str> = response_keys
        .iter()
        .chain(notification_keys.iter())
        .map(String::as_str)
        .filter(|key| {
            matches!(
                *key,
                "nextCursor"
                    | "snapshotToken"
                    | "periEntryId"
                    | "periPosition"
                    | "periLoadGeneration"
                    | "periReplay"
            )
        })
        .collect();
    assert!(
        private.is_empty(),
        "默认 caps 的连接不得收到任何 Peri 私有字段: {private:?}"
    );
    assert!(
        response.get("nextCursor").is_none() && response.get("snapshotToken").is_none(),
        "未协商连接的成功响应不得携带分页字段: {response}"
    );
}

/// 回退路径也不得误报成功：后端没有向前段读取（`Unsupported`）且回退的全量 canonical
/// 读取同样失败时，本次 load 必须以 `-32603` 结束。
#[tokio::test]
async fn s9_verification_fallback_full_read_failure_still_closes_the_load() {
    let mut fixture = ReplayFixture::new(SpyMode::UnsupportedWithFailingFullRead).await;
    fixture
        .create_session("s9-verification-fallback-fails")
        .await;
    fixture
        .append(&[BaseMessage::human("问题"), BaseMessage::ai("回答")])
        .await;
    fixture.set_caps(PeriCaps::default());
    fixture.reset_reads();

    let error = fixture
        .load()
        .await
        .expect_err("回退读取也失败时必须让本次 load 失败");
    assert_eq!(error.code, -32603, "错误码: {error:?}");
    assert_eq!(fixture.spy.segment_calls().len(), 1, "先试段读取");
    assert_eq!(fixture.spy.full_reads(), 1, "再试全量回退读取");
    assert!(fixture.updates().is_empty(), "失败路径不得留下半份首屏");
}

/// 本用例内的**第二实现**：按 ACP 投影契约从 canonical payload 推导期望 wire 事实序列。///
/// 事实格式：`text|{messageId}|{正文}` / `tool|{callId}` / `tool_update|{callId}`。
fn expected_wire_facts(payloads: &[PersistedPayload]) -> Vec<String> {
    let mut facts = Vec::new();
    for payload in payloads {
        let Some(message) = payload.as_message() else {
            continue;
        };
        let id = message.id().as_uuid().to_string();
        match message {
            BaseMessage::Human { .. } => {
                facts.push(format!("text|{id}|{}", message.content()));
            }
            BaseMessage::Ai { .. } => {
                let mut emitted: Vec<String> = Vec::new();
                if let peri_acp_types::messages::MessageContent::Blocks(blocks) =
                    message.message_content()
                {
                    for block in blocks {
                        match block {
                            ContentBlock::Reasoning { text, .. } => {
                                facts.push(format!("text|{id}|{text}"))
                            }
                            ContentBlock::Text { text } => facts.push(format!("text|{id}|{text}")),
                            ContentBlock::ToolUse { id: call, .. } => {
                                emitted.push(call.clone());
                                facts.push(format!("tool|{call}"));
                            }
                            _ => {}
                        }
                    }
                } else {
                    facts.push(format!("text|{id}|{}", message.content()));
                }
                for call in message.tool_calls() {
                    if !emitted.contains(&call.id) {
                        facts.push(format!("tool|{}", call.id));
                    }
                }
            }
            BaseMessage::Tool { tool_call_id, .. } => {
                facts.push(format!("tool_update|{tool_call_id}"));
            }
            // System：`project_message_updates` 明确返回空投影。
            _ => {}
        }
    }
    facts
}

/// 从 wire 上的 `session/update` 通知提取同一格式的事实序列（顺序即投递顺序）。
fn observed_wire_facts(fixture: &ReplayFixture) -> Vec<String> {
    let mut facts = Vec::new();
    for params in fixture.session_updates() {
        let update = &params["update"];
        match update["sessionUpdate"].as_str() {
            Some("user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk") => {
                let id = update["messageId"]
                    .as_str()
                    .expect("回放 chunk 必须携带 messageId");
                let text = update["content"]["text"].as_str().unwrap_or_default();
                facts.push(format!("text|{id}|{text}"));
            }
            Some("tool_call") => facts.push(format!(
                "tool|{}",
                update["toolCallId"].as_str().expect("tool_call 必须带身份")
            )),
            Some("tool_call_update") => facts.push(format!(
                "tool_update|{}",
                update["toolCallId"]
                    .as_str()
                    .expect("tool_call_update 必须带身份")
            )),
            _ => {}
        }
    }
    facts
}
