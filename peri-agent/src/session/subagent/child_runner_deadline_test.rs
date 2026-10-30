use super::*;

/// [回归测试] 已投递终态但真实清理未确认时，不能仅凭 active_count=0 结束。
#[tokio::test(start_paused = true)]
async fn accepted_terminal_with_pending_cleanup_hands_off_instead_of_settled() {
    let (child, manager, built, _, finished) = child_with_shell_task(false);
    assert!(manager.complete(&finished, nested_result()));
    let cleanup = manager
        .register(BgTaskRegistration {
            task_id: "cleanup-pending".into(),
            kind: BgTaskKind::Shell,
            summary: "result accepted; child process exit unconfirmed".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .unwrap();
    let mut result = nested_result();
    result.task_id = cleanup.task_id().into();
    assert!(manager.complete(&cleanup, result));
    assert_eq!(manager.active_count(), 0);
    assert!(!manager.has_pending_deliveries());
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(125),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("the cleanup obligation must reach the existing handoff deadline");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    let ChildRunOutcome::HandedOff { receipt } = outcome else {
        panic!("unconfirmed cleanup must not become Settled: {outcome:?}");
    };
    assert_eq!(receipt.tasks.len(), 1);
    assert_eq!(receipt.tasks[0].task_id, "cleanup-pending");
    manager.confirm_external_execution_stopped(&cleanup);
    assert!(!manager.has_unsettled_tasks());
}

fn install_started_window(
    context: &mut crate::agent::stages::StageContext,
) -> tokio::time::Instant {
    let window = Arc::new(ChildSettlementWindow::default());
    let deadline = window.start();
    context.async_ctx.child_settlement = Some(window);
    deadline
}

/// 完成与 D 同刻，且完成通知先被调度：activity 胜出，正常 Settled。
#[tokio::test(start_paused = true)]
async fn completion_just_before_deadline_prefers_settled() {
    let (child, manager, mut built, _, ticket) = child_with_shell_task(false);
    let deadline = install_started_window(&mut built.context);
    tokio::time::advance(Duration::from_secs(119)).await;
    let settle_manager = manager.clone();
    let settle = tokio::spawn(async move {
        tokio::time::sleep_until(deadline - Duration::from_millis(1)).await;
        assert!(settle_manager.complete(&ticket, nested_result()));
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("same-instant completion must settle before D handoff");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    settle.await.unwrap();
    assert!(matches!(outcome, ChildRunOutcome::Settled), "{outcome:?}");
}

/// 完成发生在 D 之后：deadline 先胜出，交接且不误报成功。
#[tokio::test(start_paused = true)]
async fn completion_just_after_deadline_hands_off() {
    let (child, manager, mut built, _, ticket) = child_with_shell_task(false);
    let deadline = install_started_window(&mut built.context);
    tokio::time::advance(Duration::from_secs(119)).await;
    let settle_manager = manager.clone();
    let settle = tokio::spawn(async move {
        tokio::time::sleep_until(deadline + Duration::from_millis(1)).await;
        assert!(settle_manager.complete(&ticket, nested_result()));
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("deadline must win before the later completion");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    settle.await.unwrap();
    assert!(
        matches!(outcome, ChildRunOutcome::HandedOff { .. }),
        "{outcome:?}"
    );
}

/// cancel 与 D 同刻且 cancel 先被调度：cancel 优先，不得误判为普通交接。
#[tokio::test(start_paused = true)]
async fn cancel_just_before_deadline_prefers_interrupted() {
    let (child, _manager, mut built, _, _ticket) = child_with_shell_task(false);
    let deadline = install_started_window(&mut built.context);
    tokio::time::advance(Duration::from_secs(119)).await;
    let cancel = child.config().cancel_token.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep_until(deadline - Duration::from_millis(1)).await;
        cancel.cancel();
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("cancel must interrupt before the deadline handoff");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    canceller.await.unwrap();
    assert!(
        matches!(outcome, ChildRunOutcome::Interrupted),
        "{outcome:?}"
    );
}

/// cancel 发生在 D 之后：deadline 先结束计算，Shell 不被自动取消。
#[tokio::test(start_paused = true)]
async fn cancel_just_after_deadline_hands_off_without_cancelling_shell() {
    let (child, manager, mut built, _, _ticket) = child_with_shell_task(false);
    let deadline = install_started_window(&mut built.context);
    tokio::time::advance(Duration::from_secs(119)).await;
    let cancel = child.config().cancel_token.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep_until(deadline + Duration::from_millis(1)).await;
        cancel.cancel();
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("deadline must win before the later cancel");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    canceller.await.unwrap();
    assert!(
        matches!(outcome, ChildRunOutcome::HandedOff { .. }),
        "{outcome:?}"
    );
    assert_eq!(manager.active_count(), 1, "普通 deadline 不取消 Shell");
}

/// 已准入 Act 的 tool result 只存在于 transcript；D 到期时 runner 必须交接而非 Settled。
#[tokio::test(start_paused = true)]
async fn admitted_act_tool_result_hands_off_instead_of_settled() {
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), true);
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("use a tool"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(ToolThenExpireLlm {
            calls: calls.clone(),
        }),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        child.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(
            &child.store().thread_id.clone().unwrap(),
        )),
    );
    let window = Arc::new(ChildSettlementWindow::default());
    window.start();
    built.context.async_ctx.child_settlement = Some(window);
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(200),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("runner must finish after admitted Act");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    let ChildRunOutcome::HandedOff { receipt } = outcome else {
        panic!("tool result must hand off, not settle: {outcome:?}");
    };
    assert!(receipt.tasks.is_empty());
    assert_eq!(receipt.unprocessed_tool_calls, vec!["call-1"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// 父级映射：已准入 Act tool result 的交接必须 success=false。
#[tokio::test(start_paused = true)]
async fn admitted_act_tool_result_parent_result_is_failure() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let child = make_child(manager, true);
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("use a tool"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(ToolThenExpireLlm {
            calls: calls.clone(),
        }),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        child.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(
            &child.store().thread_id.clone().unwrap(),
        )),
    );
    let window = Arc::new(ChildSettlementWindow::default());
    window.start();
    built.context.async_ctx.child_settlement = Some(window);
    let child_id = child.store().thread_id.clone().unwrap();
    let task_id = crate::session::subagent::run_sync::new_sync_task_id();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = parking_lot::Mutex::new(Some(delivered_tx));
    let run_id = child_id.clone();
    let run_session = child.clone();
    let call = tokio::spawn(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            child.clone(),
            child_id.clone(),
            "fixture".into(),
            Some(parent_manager),
            Some(task_id),
            Some(Arc::new(
                move |result: &crate::agent::events::BackgroundTaskResult, _| {
                    if let Some(sender) = delivered_tx.lock().take() {
                        let _ = sender.send(result.clone());
                    }
                    Ok(())
                },
            )),
            move |handoff, _| async move {
                crate::session::subagent::run_sync::run_sync_subagent(
                    &run_id,
                    "fixture",
                    "/tmp",
                    10,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    built,
                    run_session,
                    None,
                    Some(handoff),
                )
                .await
            },
        ),
    );
    let completion = call.await.unwrap().unwrap();
    let crate::session::subagent::run_sync::SyncCompletion::Pending(pending) = completion else {
        panic!("admitted Act tool result must detach as Pending");
    };
    assert_eq!(pending.receipt.unprocessed_tool_calls, vec!["call-1"]);
    let delivered = tokio::time::timeout(Duration::from_secs(5), delivered_rx)
        .await
        .expect("parent result must be delivered")
        .unwrap();
    assert!(!delivered.success);
    assert!(!delivered.timed_out);
    assert!(delivered.output.contains("status: incomplete"));
    assert!(delivered.output.contains("call-1"));
}

struct StopAfterTool;

#[async_trait::async_trait]
impl BaseTool for StopAfterTool {
    fn name(&self) -> &str {
        "StopAfterTool"
    }
    fn description(&self) -> &str {
        "fixture: runs once then requests hook stop"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("tool result before stop".into())
    }
}

struct StopAfterBatch;

#[async_trait::async_trait]
impl crate::middleware::Middleware for StopAfterBatch {
    fn name(&self) -> &str {
        "post_tool_batch_stop"
    }

    async fn after_tools_batch(
        &self,
        state: &mut dyn crate::middleware::capabilities::AfterToolsBatchState,
        results: &[(ToolCall, crate::agent::react::ToolResult)],
    ) -> crate::error::AgentResult<()> {
        use peri_acp_types::system_reminder::{
            ReminderCategory, ReminderDelivery, ReminderSeverity,
        };
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.id, "call-1");
        assert!(!results[0].1.is_error, "fixture 工具必须实际执行成功");
        state.enqueue_stop_intent(crate::session::producer_reminders::trusted_reminder(
            ReminderCategory::Lifecycle,
            "hook",
            "post_tool_batch_stop",
            ReminderSeverity::Info,
            ReminderDelivery::Configurable,
            "PostToolBatch 请求停止".into(),
            None,
            serde_json::json!({"continue": false}),
        ));
        Ok(())
    }
}

struct ToolCallOnceLlm {
    calls: Arc<AtomicUsize>,
    tool_name: &'static str,
}

#[async_trait::async_trait]
impl ReactLLM for ToolCallOnceLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        assert_eq!(
            self.calls.fetch_add(1, Ordering::SeqCst),
            0,
            "no second Reason"
        );
        Ok(Reasoning::with_tools(
            "call the tool",
            vec![ToolCall::new(
                "call-1",
                self.tool_name,
                serde_json::json!({}),
            )],
        ))
    }
}

#[tokio::test(start_paused = true)]
async fn post_tool_batch_stop_preserves_act_receipt() {
    assert_act_receipt(true).await;
}

#[tokio::test(start_paused = true)]
async fn budget_exhaustion_preserves_act_receipt() {
    assert_act_receipt(false).await;
}

/// 直接经过 runner 验证：无其他任务、消息义务时，停止与预算耗尽都保留 Act 身份。
async fn assert_act_receipt(stop: bool) {
    let manager = Arc::new(TaskManager::new());
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(uuid::Uuid::now_v7().to_string()),
    );
    let (owner, store) = bind_owner_store(&child, &manager);
    child.set_subagent_host(SubagentHost {
        task_manager: Some(manager.clone()),
        child_owner: Some(owner),
        ..Default::default()
    });
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("执行工具"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut chain = MiddlewareChain::new();
    if stop {
        chain.add(Box::new(StopAfterBatch));
    }
    let built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(ToolCallOnceLlm {
            calls: calls.clone(),
            tool_name: "StopAfterTool",
        }),
        Arc::new(chain),
        vec![Arc::new(StopAfterTool)],
        Arc::new(|_| true),
        None,
        "/tmp",
        child.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(
            &child.store().thread_id.clone().unwrap(),
        )),
    );
    let permit = permit_for(&child, &built.context).unwrap();
    let outcome = run_child_until_terminal(built.context, 1, &child).await;
    permit.finish(Ok(()));
    let receipt = match outcome {
        ChildRunOutcome::HandedOff { receipt } if stop => {
            assert_eq!(receipt.reason, ChildHandoffReason::HookStop);
            receipt
        }
        ChildRunOutcome::Failed {
            error: crate::error::AgentError::MaxIterationsExceeded(1),
            residual: Some(receipt),
        } if !stop => {
            assert_eq!(receipt.reason, ChildHandoffReason::ExecutionFailed);
            receipt
        }
        other => panic!("停止或预算耗尽必须保留已执行的工具身份: {other:?}"),
    };
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(manager.active_count(), 0);
    assert!(!manager.has_pending_deliveries());
    assert!(!child.queue().has_required());
    assert!(receipt.tasks.is_empty());
    assert!(receipt.message_ids.is_empty());
    assert_eq!(receipt.unprocessed_tool_calls, vec!["call-1"]);
    assert_eq!(
        crate::session::subagent::count_tool_calls_from_session(&child),
        1
    );
    let payloads = store.payloads();
    let reminder = payloads
        .iter()
        .find_map(|payload| match payload {
            peri_acp_types::store::PersistedPayload::SystemReminder { id, reminder }
                if *id == receipt.canonical_message_id =>
            {
                Some(reminder.as_reminder())
            }
            _ => None,
        })
        .expect("canonical 交接消息必须落库");
    assert_eq!(
        reminder.metadata["unprocessed_tool_calls"],
        serde_json::json!(receipt.unprocessed_tool_calls)
    );
    assert!(reminder.body.contains(&format!(
        "未推理 tool calls: {}",
        receipt.unprocessed_tool_calls.join(", ")
    )));
    let notice = crate::session::subagent::util::format_handoff_notice(&receipt);
    assert!(notice.contains(&format!(
        "unprocessed_tool_calls: {}",
        receipt.unprocessed_tool_calls.join(", ")
    )));
    assert!(notice.contains("status: incomplete"));
}

/// HookStop 在 Act 之后到达：无其他义务，仍须交接 tool result；只调用一次模型。
#[tokio::test(start_paused = true)]
async fn hook_stop_after_act_hands_off_tool_result_and_parent_fails() {
    let manager = Arc::new(TaskManager::new());
    let parent_manager = Arc::new(TaskManager::new());
    let child = make_child(manager, true);
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run a tool then stop"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(ToolCallOnceLlm {
            calls: calls.clone(),
            tool_name: "StopAfterTool",
        }),
        Arc::new({
            let mut chain = MiddlewareChain::new();
            chain.add(Box::new(StopAfterBatch));
            chain
        }),
        vec![Arc::new(StopAfterTool)],
        Arc::new(|_| true),
        None,
        "/tmp",
        child.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(
            &child.store().thread_id.clone().unwrap(),
        )),
    );
    let child_id = child.store().thread_id.clone().unwrap();
    let task_id = crate::session::subagent::run_sync::new_sync_task_id();
    let (delivered_tx, delivered_rx) = tokio::sync::oneshot::channel();
    let delivered_tx = parking_lot::Mutex::new(Some(delivered_tx));
    let run_id = child_id.clone();
    let run_session = child.clone();
    let call = tokio::spawn(
        crate::session::subagent::run_sync::run_sync_with_registered_handoff(
            child.clone(),
            child_id,
            "fixture".into(),
            Some(parent_manager),
            Some(task_id),
            Some(Arc::new(
                move |result: &crate::agent::events::BackgroundTaskResult, _| {
                    if let Some(sender) = delivered_tx.lock().take() {
                        let _ = sender.send(result.clone());
                    }
                    Ok(())
                },
            )),
            move |handoff, _| async move {
                crate::session::subagent::run_sync::run_sync_subagent(
                    &run_id,
                    "fixture",
                    "/tmp",
                    10,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    built,
                    run_session,
                    None,
                    Some(handoff),
                )
                .await
            },
        ),
    );
    let completion = call.await.unwrap().unwrap();
    let crate::session::subagent::run_sync::SyncCompletion::Pending(pending) = completion else {
        panic!("HookStop with unprocessed tool result must detach as Pending");
    };
    assert_eq!(pending.receipt.reason, ChildHandoffReason::HookStop);
    assert_eq!(pending.receipt.unprocessed_tool_calls, vec!["call-1"]);
    let delivered = tokio::time::timeout(Duration::from_secs(5), delivered_rx)
        .await
        .expect("parent result must be delivered")
        .unwrap();
    assert!(!delivered.success);
    assert!(!delivered.timed_out);
    assert!(delivered.output.contains("status: incomplete"));
    assert!(delivered.output.contains("call-1"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no second Reason");
}
