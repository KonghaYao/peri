// Responsibility-specific cases share the original test namespace and fixtures.

// ── is_keepgoing: 跨层判空契约测试 ───────────────────────────────────────
//
// 与 peri-agent stages_test 的
// `test_append_messages_empty_prompt_skipped` / `test_append_messages_whitespace_prompt_kept`
// 成对，双侧锁定 ARC-KEEPGOING-001 的判空语义（`MessageContent::is_empty()`）。

/// 空文本 → keepgoing（TUI keepgoing 按钮的真实 payload 是 `text("")`）
#[test]
fn test_is_keepgoing_empty_text() {
    assert!(crate::session::exec::executor::is_keepgoing(
        &MessageContent::text("")
    ));
}

/// 纯空白文本不算空 content block → 非 keepgoing（用户输入空格应正常跑 loop）
#[test]
fn test_is_keepgoing_whitespace_text_not_keepgoing() {
    assert!(!crate::session::exec::executor::is_keepgoing(
        &MessageContent::text("   ")
    ));
}

/// 纯附件消息（Blocks([Image])）不是空 → 非 keepgoing（trim 判空会把图片误判）
#[test]
fn test_is_keepgoing_image_block_not_keepgoing() {
    let content = MessageContent::blocks(vec![ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".to_string(),
            data: "fake".to_string(),
        },
    }]);
    assert!(!crate::session::exec::executor::is_keepgoing(&content));
}

// ── run_session_loop: keepgoing 短路路径 TRAP 验证 ────────────────────────

/// [TRAP] keepgoing 短路路径（空历史 + 空 prompt）必须调用 `push_done`，
/// 否则 TUI 依赖 AgentDone→TurnDone 退出 loading 的机制失效，界面永久卡在
/// loading（ARC-EVENT-001 / ARC-KEEPGOING-001）。
#[tokio::test]
async fn test_run_session_loop_keepgoing_short_circuit_calls_push_done() {
    // Arrange
    let mock_sink = Arc::new(MockEventSink::new());
    let ctx = make_session_context("test-session");
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text(""),
        false,
        vec![],
    );
    let turn = TurnInput {
        // keepgoing 语义：不注入 recall（否则 recall 拼进 user 消息使其非空）
        incoming_recalls: vec!["should-be-skipped".to_string()],
        ..turn
    };

    // Act
    let result = crate::session::exec::executor::run_session_loop(ctx, turn).await;

    // Assert
    assert!(result.ok);
    assert_eq!(
        result.stop_reason,
        PromptStopReason::EndTurn,
        "短路返回的 stop_reason 必须为 EndTurn"
    );
    assert!(
        result.recall_items.is_empty(),
        "keepgoing 短路不应产生 recall items"
    );
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "keepgoing 短路路径必须调用 push_done 一次（TRAP: TUI 永久 loading）"
    );
    assert_eq!(
        mock_sink.last_push_done_stop_reason().as_deref(),
        Some("end_turn"),
        "keepgoing 短路的协议出口终态必须唯一且为 end_turn"
    );
}

/// Issue 2026-08-05 返工链路验证：keepgoing 短路路径的 push_done 必须透传
/// SessionContext.request_id（服务器回带 → TUI stale TurnInterrupted 配对）。
#[tokio::test]
async fn test_run_session_loop_keepgoing_short_circuit_forwards_request_id() {
    // Arrange
    let mock_sink = Arc::new(MockEventSink::new());
    let mut ctx = make_session_context("test-session");
    ctx.request_id = Some("req-1".to_string());
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text(""),
        false,
        vec![],
    );

    // Act
    let result = crate::session::exec::executor::run_session_loop(ctx, turn).await;

    // Assert
    assert!(result.ok);
    assert_eq!(
        mock_sink.last_push_done_request_id().as_deref(),
        Some("req-1"),
        "push_done 必须透传 SessionContext.request_id"
    );
}
