//! 手动 compact 的持久化行为与输入一致性校验。

use super::*;

#[tokio::test]
async fn test_compact_pipeline_uses_bound_sqlite_lifecycle() {
    let session = BoundSession::open("compact-pipeline.db").await;
    let history = vec![
        BaseMessage::system("pipeline system prompt"),
        BaseMessage::human("pipeline user question"),
        BaseMessage::ai("pipeline assistant response"),
    ];
    session.append(&history).await;

    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx_with_model_and_thread(
        sink,
        history.clone(),
        session.cwd.clone(),
        Arc::new(MockSummaryModel::new(
            "<summary>PIPELINE_LIFECYCLE_MARKER</summary>",
        )),
        Some(session.resources()),
        Some(session.thread_id.clone()),
    );

    let result = execute_compact(&CompactCommand, ctx).await;

    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert!(
        result
            .messages
            .iter()
            .any(|message| message.content().contains("PIPELINE_LIFECYCLE_MARKER")),
        "成功结果必须含 summary"
    );
    let stored_history = session.stored_messages().await;
    assert!(
        stored_history
            .iter()
            .any(|message| message.content().contains("PIPELINE_LIFECYCLE_MARKER")),
        "绑定 store 的 lifecycle 必须持久化 summary"
    );
    let flags = session.stored_flags().await;
    assert!(!flags.contains_key(&history[0].id()), "System 不得被排除");
    assert!(flags[&history[1].id()].excluded, "Human 必须被排除");
    assert!(flags[&history[2].id()].excluded, "AI 必须被排除");
}

#[tokio::test]
async fn test_compact_pipeline_does_not_append_preexisting_history_to_bound_thread() {
    let session = BoundSession::open("compact-existing-history.db").await;
    let history = vec![
        BaseMessage::human("already persisted user question"),
        BaseMessage::ai("already persisted assistant response"),
    ];
    session.append(&history).await;

    let ctx = make_ctx_with_model_and_thread(
        Arc::new(MockEventSink::new()),
        history.clone(),
        session.cwd.clone(),
        Arc::new(MockSummaryModel::new(
            "<summary>EXISTING_HISTORY_NOT_DUPLICATED</summary>",
        )),
        Some(session.resources()),
        Some(session.thread_id.clone()),
    );

    let result = execute_compact(&CompactCommand, ctx).await;

    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    let stored_history = session.stored_messages().await;
    assert_eq!(
        stored_history.len(),
        history.len() + 1,
        "已持久化的原始 history 不得在 compact 时被重复写入"
    );
    for original in &history {
        assert_eq!(
            stored_history
                .iter()
                .filter(|message| message.id() == original.id())
                .count(),
            1,
            "原始消息 {:?} 在 SQLite thread 中必须仅出现一次",
            original.id()
        );
    }
    assert!(
        stored_history.iter().any(|message| message
            .content()
            .contains("EXISTING_HISTORY_NOT_DUPLICATED")),
        "compact 后必须追加 summary"
    );
}

#[tokio::test]
async fn test_compact_pipeline_reuses_canonical_history_for_second_bound_sqlite_lifecycle() {
    let session = BoundSession::open("compact-second-lifecycle.db").await;
    let history = vec![
        BaseMessage::system("persistent system prompt"),
        BaseMessage::human("first compact request"),
        BaseMessage::ai("first compact response"),
    ];
    session.append(&history).await;

    let first_sink = Arc::new(MockEventSink::new());
    let first = execute_compact(
        &CompactCommand,
        make_ctx_with_model_and_thread(
            first_sink.clone(),
            history,
            session.cwd.clone(),
            Arc::new(MockSummaryModel::new(
                "<summary>FIRST_COMPACT_LIFECYCLE_SUMMARY</summary>",
            )),
            Some(session.resources()),
            Some(session.thread_id.clone()),
        ),
    )
    .await;
    assert_eq!(first.stop_reason, PromptStopReason::EndTurn);
    assert!(
        first.messages.iter().any(|message| message
            .content()
            .contains("FIRST_COMPACT_LIFECYCLE_SUMMARY")),
        "首次 compact 必须产生 summary"
    );

    let second_sink = Arc::new(MockEventSink::new());
    let second = execute_compact(
        &CompactCommand,
        make_ctx_with_model_and_thread(
            second_sink.clone(),
            session.stored_messages().await,
            session.cwd.clone(),
            Arc::new(MockSummaryModel::new(
                "<summary>SECOND_COMPACT_LIFECYCLE_SUMMARY</summary>",
            )),
            Some(session.resources()),
            Some(session.thread_id.clone()),
        ),
    )
    .await;

    assert_eq!(
        second.stop_reason,
        PromptStopReason::EndTurn,
        "第二次 compact 必须完成而非因 physical/visible history 不匹配被拒绝"
    );
    assert!(
        second_sink
            .events()
            .iter()
            .any(|(_, json)| json.contains("compact_completed")),
        "第二次 compact 必须发出 CompactCompleted，而非只以 EndTurn 返回错误"
    );
    assert!(
        second.messages.iter().any(|message| message
            .content()
            .contains("SECOND_COMPACT_LIFECYCLE_SUMMARY")),
        "第二次 compact 必须返回新的 summary"
    );
    let stored = session.stored_messages().await;
    assert_eq!(
        stored
            .iter()
            .filter(|message| message.content().contains("_COMPACT_LIFECYCLE_SUMMARY"))
            .count(),
        2,
        "同一 thread 的两次 compact 必须各自持久化一个 summary"
    );
}

#[tokio::test]
async fn test_compact_pipeline_history_read_only_lifecycle_failure_preserves_durable_message_ids() {
    // 迁前本测试用 FilesystemThreadStore（能力面 = 只读历史）。新契约下等价的能力面
    // 是只读打开的同一库：数据可读、`DataCapabilities::HistoryReadOnly`，完整
    // lifecycle 无法完成。
    let session = BoundSession::open("compact-read-only-lifecycle.db").await;
    let history = vec![
        BaseMessage::human("read-only compact request"),
        BaseMessage::ai("read-only compact response"),
    ];
    session.append(&history).await;
    let before_ids = session
        .stored_messages()
        .await
        .iter()
        .map(BaseMessage::id)
        .collect::<Vec<_>>();
    let read_only = session.open_read_only().await;

    let result = execute_compact(
        &CompactCommand,
        make_ctx_with_model_and_thread(
            Arc::new(MockEventSink::new()),
            history.clone(),
            session.cwd.clone(),
            Arc::new(MockSummaryModel::new(
                "<summary>READ_ONLY_LIFECYCLE_MUST_NOT_APPEND</summary>",
            )),
            Some(read_only),
            Some(session.thread_id.clone()),
        ),
    )
    .await;

    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(
        result
            .messages
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        history.iter().map(BaseMessage::id).collect::<Vec<_>>(),
        "只读能力面下 lifecycle 不受支持时必须返回原始 history"
    );
    assert_eq!(
        session
            .stored_messages()
            .await
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        before_ids,
        "pipeline 的 preliminary append 不得向只读后端写入重复 message IDs"
    );
}

#[tokio::test]
async fn test_compact_pipeline_rejects_incoming_history_that_differs_from_bound_thread() {
    let session = BoundSession::open("compact-history-mismatch.db").await;
    let stored_history = vec![
        BaseMessage::human("stored user question"),
        BaseMessage::ai("stored assistant response"),
    ];
    let incoming_history = vec![
        BaseMessage::human("incoming user question"),
        BaseMessage::ai("incoming assistant response"),
    ];
    assert_ne!(
        stored_history
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        incoming_history
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        "fixture 的 stored 与 incoming history 必须具有不同 ID"
    );
    session.append(&stored_history).await;

    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx_with_model_and_thread(
        sink.clone(),
        incoming_history.clone(),
        session.cwd.clone(),
        Arc::new(MockSummaryModel::new(
            "<summary>HISTORY_MISMATCH_MUST_NOT_BE_PERSISTED</summary>",
        )),
        Some(session.resources()),
        Some(session.thread_id.clone()),
    );

    let result = execute_compact(&CompactCommand, ctx).await;

    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(
        result
            .messages
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        incoming_history
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        "持久化 context 与传入 history ID 不一致时必须原样返回 incoming history"
    );
    let fb = result.feedback.as_ref().expect("不匹配时应携带 feedback");
    assert_eq!(fb.level, FeedbackLevel::Error);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert_eq!(fb.message, "compact persistence context mismatch");
    assert!(
        sink.events().is_empty(),
        "命令自身不应发射 CompactError 事件（Phase 5 Step 4 收敛为 feedback）"
    );

    let persisted_history = session.stored_messages().await;
    assert_eq!(
        persisted_history
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        stored_history
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        "不匹配时 store 必须保持仅含 stored history"
    );
    assert!(
        !persisted_history.iter().any(|message| message
            .content()
            .contains("HISTORY_MISMATCH_MUST_NOT_BE_PERSISTED")),
        "不匹配时不得持久化 summary"
    );
    assert!(
        session.stored_flags().await.is_empty(),
        "不匹配时不得写入 message flags"
    );
}

#[tokio::test]
async fn test_compact_pipeline_without_thread_binding_returns_error_without_mutating_history() {
    let history = vec![
        BaseMessage::human("unbound user question"),
        BaseMessage::ai("unbound assistant response"),
    ];
    let sink = Arc::new(MockEventSink::new());
    let ctx = make_ctx_with_model_and_thread(
        sink.clone(),
        history.clone(),
        "/tmp".to_string(),
        Arc::new(MockSummaryModel::new(
            "<summary>UNBOUND_MUST_NOT_COMPACT</summary>",
        )),
        None,
        None,
    );

    let result = execute_compact(&CompactCommand, ctx).await;

    assert_eq!(result.stop_reason, PromptStopReason::EndTurn);
    assert_eq!(
        result
            .messages
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        history.iter().map(BaseMessage::id).collect::<Vec<_>>(),
        "缺少 store/thread binding 时必须保留原 history"
    );
    let fb = result
        .feedback
        .as_ref()
        .expect("缺少 binding 应携带 feedback");
    assert_eq!(fb.level, FeedbackLevel::Error);
    assert_eq!(fb.channel, FeedbackChannel::UiOnly);
    assert_eq!(fb.message, "compact persistence is unavailable");
    assert!(
        sink.events().is_empty(),
        "命令自身不应发射 CompactError 事件（Phase 5 Step 4 收敛为 feedback）"
    );
}
