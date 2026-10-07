use super::*;

// ── run_session_loop: AsyncContinuation 内部续跑（非 keepgoing）─────────────

/// [AsyncContinuation] 内部续跑（continuation=true）不把空 user prompt 当
/// keepgoing：空历史 + 空 prompt 仍进入 agent 管线（绕过 keepgoing 空历史
/// short-circuit——后者会直接返回 ok=true/EndTurn）。
#[tokio::test]
async fn test_continuation_bypasses_keepgoing_short_circuit() {
    // Arrange：预取消 token，保证进入管线后快速中断（不触发真实 LLM 调用）
    let ctx = make_session_context("test-continuation").await;
    ctx.cancel.cancel();
    let stage_build = make_stage_build(&ctx);
    let mock_sink = Arc::new(MockEventSink::new());
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text(""),
        true,
        vec![],
        stage_build,
    );

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert：未走 keepgoing 短路（短路会返回 ok=true/EndTurn 且不构建 agent），
    // 而是进入管线后被预取消 token 中断（ok=false/Cancelled）。
    assert!(!result.ok, "continuation 不得走 keepgoing 空历史短路");
    assert_eq!(
        result.stop_reason,
        PromptStopReason::Cancelled,
        "进入管线后被预取消 token 中断"
    );
}

/// [Seam 2 / 验收⑤] turn 终态唯一 + terminal 事件位于 turn 全部输出之后。
///
/// §9 事件契约（docs/top-level.md）：terminal 事件必须位于该 turn 全部输出
/// 事件之后；turn 终态唯一（Completed 或 Interrupted）。本测试走预取消中断
/// 路径（Interrupted 终态）：断言 TurnStarted/TurnEnded 各恰好一次、
/// TurnEnded 是事件流最后一条且 status=Interrupted、协议出口 push_done
/// 恰好一次且 stop_reason=cancelled（与 TurnEnded 语义一致）。
#[tokio::test]
async fn test_turn_terminal_state_unique_and_last() {
    // Arrange：预取消 token，进入管线后立即中断（不触发真实 LLM 调用）
    let mock_sink = Arc::new(MockEventSink::new());
    let ctx = make_session_context("test-turn-terminal").await;
    ctx.cancel.cancel();
    let stage_build = make_stage_build(&ctx);
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text(""),
        true,
        vec![],
        stage_build,
    );

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert：终态唯一（Interrupted）
    assert!(!result.ok);
    assert_eq!(result.stop_reason, PromptStopReason::Cancelled);

    // terminal 事件唯一且位于全部输出之后
    let events = mock_sink.pushed_events.lock().unwrap();
    assert!(
        !events.is_empty(),
        "进入管线后应产生事件流（至少 TurnStarted + TurnEnded）"
    );
    let started = events
        .iter()
        .filter(|e| e.contains("\"turn_started\""))
        .count();
    let ended = events
        .iter()
        .filter(|e| e.contains("\"turn_ended\""))
        .count();
    assert_eq!(started, 1, "每个 turn 恰好一个 TurnStarted");
    assert_eq!(ended, 1, "每个 turn 恰好一个 terminal 事件（终态唯一）");
    let last = events.last().expect("事件流非空");
    assert!(
        last.contains("\"turn_ended\"") && last.contains("interrupted"),
        "terminal 事件必须位于该 turn 全部输出之后且 status=Interrupted: {last}"
    );
    drop(events);

    // 协议出口终态唯一，且与 TurnEnded 语义一致
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "终态信号（push_done）必须恰好一次"
    );
    assert_eq!(
        mock_sink
            .push_done_stop_reasons
            .lock()
            .unwrap()
            .last()
            .cloned(),
        Some("cancelled".to_string()),
        "push_done 终态与 TurnEnded(Interrupted) 语义一致"
    );
}

/// Root LlmCallEnd must cross the real forwarder before the session exposes
/// AgentDone/push_done. The gate holds the final usage event inside the
/// forwarder and proves the session cannot complete early.
#[cfg(not(windows))]
#[tokio::test]
async fn test_forwarder_barrier_orders_final_usage_before_done() {
    let model: Arc<dyn Model> = Arc::new(UsageModel);
    let mut ctx = make_session_context("test-forwarder-usage-barrier").await;
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let stage_build = make_stage_build(&ctx);
    let sink = Arc::new(MockEventSink::new());
    let mut turn = make_turn_input(
        Arc::clone(&sink) as Arc<dyn EventSink>,
        MessageContent::text("finish with usage"),
        false,
        vec![],
        stage_build,
    );
    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    turn.forwarder_launcher = make_gated_forwarder_launcher(reached_tx, release_rx);

    let task = tokio::spawn(async move { run_session_loop(ctx, turn).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), reached_rx)
        .await
        .expect("forwarder must observe final LlmCallEnd")
        .expect("barrier signal must remain connected");
    assert!(
        !task.is_finished(),
        "session must not publish terminal completion while final usage is gated"
    );
    release_tx.send(()).expect("release barrier");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("session completes after usage release")
        .expect("session task must not panic");
    assert!(result.ok);

    let operations = sink.operations.lock().unwrap();
    let usage_index = operations
        .iter()
        .position(|operation| operation.contains("llm_call_end"))
        .expect("final LlmCallEnd must reach the event sink");
    let done_index = operations
        .iter()
        .position(|operation| operation == "done:end_turn")
        .expect("session must publish done");
    assert!(
        usage_index < done_index,
        "UsageUpdate source must precede done"
    );
}

#[cfg(not(windows))]
#[tokio::test]
async fn test_forwarder_join_error_fails_turn_before_done_without_late_usage() {
    let model: Arc<dyn Model> = Arc::new(UsageModel);
    let mut ctx = make_session_context("test-forwarder-join-error").await;
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let stage_build = make_stage_build(&ctx);
    let sink = Arc::new(MockEventSink::new());
    let mut turn = make_turn_input(
        Arc::clone(&sink) as Arc<dyn EventSink>,
        MessageContent::text("finish with failed forwarder"),
        false,
        vec![],
        stage_build,
    );
    turn.forwarder_launcher = make_aborting_forwarder_launcher();

    let result = run_session_loop(ctx, turn).await;
    assert!(!result.ok);
    assert!(result.failure.is_some());
    assert!(
        result.messages.iter().any(|message| {
            matches!(message, BaseMessage::Human { .. })
                && message.content().contains("finish with failed forwarder")
        }),
        "forwarder failure must preserve the current user message in PromptResult history"
    );
    assert!(
        result.messages.iter().any(|message| {
            matches!(message, BaseMessage::Ai { .. }) && message.content().contains("done")
        }),
        "forwarder failure must preserve the completed assistant message in PromptResult history"
    );

    let events: Vec<ExecutorEvent> = sink
        .pushed_events
        .lock()
        .unwrap()
        .iter()
        .map(|json| serde_json::from_str(json).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::TurnStarted { .. }))
            .count(),
        1,
        "forwarder failure must retain the unique TurnStarted"
    );
    let ended: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, ExecutorEvent::TurnEnded { .. }))
        .collect();
    assert_eq!(ended.len(), 1, "forwarder failure needs one TurnEnded");
    let ExecutorEvent::TurnEnded {
        status, error_kind, ..
    } = ended[0]
    else {
        unreachable!("filtered to TurnEnded")
    };
    assert_eq!(*status, peri_acp_types::event::TurnStatus::Error);
    assert_eq!(
        *error_kind,
        Some(peri_acp_types::event::TurnErrorKind::Internal)
    );

    let operations = sink.operations.lock().unwrap();
    let failure_index = operations
        .iter()
        .position(|operation| operation.contains("agent_execution_failed"))
        .expect("join error must publish AgentExecutionFailed");
    let terminal_index = operations
        .iter()
        .position(|operation| operation.contains("turn_ended"))
        .expect("join error must publish TurnEnded(Error/Internal)");
    let done_index = operations
        .iter()
        .position(|operation| operation == "done:end_turn")
        .expect("failed session must still terminate");
    assert!(failure_index < terminal_index && terminal_index < done_index);
    assert!(
        operations[done_index + 1..]
            .iter()
            .all(|operation| !operation.contains("llm_call_end")),
        "no UsageUpdate source may arrive after AgentDone"
    );
}

/// [回归测试] 取消发生在真实 ModelStream 已返回之后，ACP 仍必须
/// 对 PromptResult、TurnEnded 和 push_done 给出唯一且一致的 cancelled 终态。
#[cfg(not(windows))]
#[tokio::test]
async fn test_cancel_during_reason_has_one_interrupted_terminal() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let model: Arc<dyn Model> = Arc::new(CancelGateModel {
        entered: Mutex::new(Some(entered_tx)),
    });
    let mut ctx = make_session_context("test-cancel-during-reason").await;
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let cancel = ctx.cancel.clone();
    let stage_build = make_stage_build(&ctx);
    let sink = Arc::new(MockEventSink::new());
    let turn = make_turn_input(
        Arc::clone(&sink) as Arc<dyn EventSink>,
        MessageContent::text("cancel while reasoning"),
        false,
        vec![],
        stage_build,
    );
    let task = tokio::spawn(async move { run_session_loop(ctx, turn).await });
    entered_rx.await.expect("primary model stream 必须已返回");

    cancel.cancel();
    let result = task.await.expect("session loop task 不得 panic");

    assert!(!result.ok);
    assert_eq!(result.stop_reason, PromptStopReason::Cancelled);
    assert!(result.failure.is_none(), "用户取消不得产生 fatal failure");
    let events: Vec<ExecutorEvent> = sink
        .pushed_events
        .lock()
        .unwrap()
        .iter()
        .map(|json| serde_json::from_str(json).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::TurnStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::TurnEnded { .. }))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(
        event,
        ExecutorEvent::TurnEnded {
            status: peri_acp_types::event::TurnStatus::Interrupted,
            error_kind: Some(peri_acp_types::event::TurnErrorKind::Interrupted),
            ..
        }
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, ExecutorEvent::AgentExecutionFailed { .. })));
    assert_eq!(sink.push_done_count(), 1);
    assert_eq!(
        sink.push_done_stop_reasons.lock().unwrap().as_slice(),
        ["cancelled"]
    );
}

/// fatal 终态的兼容 failure 事件必须在唯一 TurnEnded 之前，
/// 且与 PromptResult 共用同一份脱敏文案；done 仅在事件泵排空后发送。
#[cfg(not(windows))]
#[tokio::test]
async fn test_fatal_failure_precedes_turn_end_and_done() {
    let model: Arc<dyn Model> = Arc::new(FatalModel);
    let mut ctx = make_session_context("test-fatal-terminal-order").await;
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model)));
    let stage_build = make_stage_build(&ctx);
    let sink = Arc::new(MockEventSink::new());
    let turn = make_turn_input(
        Arc::clone(&sink) as Arc<dyn EventSink>,
        MessageContent::text("trigger fatal model error"),
        false,
        vec![],
        stage_build,
    );

    let result = run_session_loop(ctx, turn).await;

    let failure = result.failure.expect("fatal model error 必须产生 failure");
    assert_eq!(
        failure.kind,
        peri_acp_types::session::ExecutionFailureKind::LlmHttp
    );
    assert_eq!(failure.http_status, Some(500));
    assert!(failure.public_message.contains("safe-request-id"));
    let operations = sink.operations.lock().unwrap();
    let failure_index = operations
        .iter()
        .position(|operation| operation.contains("agent_execution_failed"))
        .expect("必须发射 AgentExecutionFailed");
    let turn_end_index = operations
        .iter()
        .position(|operation| operation.contains("turn_ended"))
        .expect("必须发射 TurnEnded");
    let done_index = operations
        .iter()
        .position(|operation| operation == "done:end_turn")
        .expect("必须在 pump drain 后 push_done");
    assert!(failure_index < turn_end_index && turn_end_index < done_index);
    assert_eq!(
        operations
            .iter()
            .filter(|operation| operation.contains("turn_ended"))
            .count(),
        1
    );
    let failure_event: ExecutorEvent = serde_json::from_str(&operations[failure_index]).unwrap();
    match failure_event {
        ExecutorEvent::AgentExecutionFailed { message } => {
            assert_eq!(message, failure.public_message)
        }
        other => panic!("预期 AgentExecutionFailed，got: {other:?}"),
    }
    let turn_end: ExecutorEvent = serde_json::from_str(&operations[turn_end_index]).unwrap();
    assert!(matches!(
        turn_end,
        ExecutorEvent::TurnEnded {
            status: peri_acp_types::event::TurnStatus::Error,
            error_kind: Some(peri_acp_types::event::TurnErrorKind::LlmFailure),
            ..
        }
    ));
}

/// [AsyncContinuation] 内部续跑不写入空 human prompt：Phase 6 跳过 Prompt push，
/// v2 MessageQueue 不出现消息；对比 keepgoing（非空历史）会 push 一条空 Prompt。
#[tokio::test]
async fn test_continuation_skips_empty_prompt_push() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_id = "test-continuation-queue";
    let (ctx, sm) = make_session_context_with_manager(session_id, &tmp).await;
    ctx.cancel.cancel();
    let stage_build = make_stage_build(&ctx);
    let history = vec![BaseMessage::human("prior turn")];

    // Act 1：continuation=true（空 content + 非空历史）
    let turn = make_turn_input(
        Arc::new(MockEventSink::new()) as Arc<dyn EventSink>,
        MessageContent::text(""),
        true,
        history.clone(),
        stage_build.clone(),
    );
    let _ = run_session_loop(ctx, turn).await;

    // Assert 1：队列无任何消息（未写空 human）
    let queue = sm
        .get_session(session_id)
        .expect("session 应存在")
        .v2_message_queue
        .clone();
    assert!(
        queue.drain_all().is_empty(),
        "continuation 不得向 v2 queue 写入空 human prompt"
    );

    // Act 2：keepgoing（continuation=false，同为空 content）——对比组
    let mut ctx2 = make_session_context(session_id).await;
    ctx2.session_access =
        Some(Arc::new(sm.clone()) as Arc<dyn peri_acp_types::session::SessionAccessPort>);
    ctx2.cancel.cancel();
    let stage_build2 = make_stage_build(&ctx2);
    let turn2 = make_turn_input(
        Arc::new(MockEventSink::new()) as Arc<dyn EventSink>,
        MessageContent::text(""),
        false,
        history,
        stage_build2,
    );
    let _ = run_session_loop(ctx2, turn2).await;

    // Assert 2：keepgoing 也不得虚构空 human publication。
    let drained = sm
        .get_session(session_id)
        .expect("session 应存在")
        .v2_message_queue
        .clone()
        .drain_all();
    assert!(drained.is_empty(), "keepgoing 不得虚构空 human publication");
}
