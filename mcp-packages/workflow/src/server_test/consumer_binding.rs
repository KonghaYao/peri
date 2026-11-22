/// 装配期时序（design §6.2）：实例在会话环境装配期 spawn，早于会话旧链构造
/// `WorkflowRuntime`；槽位未绑定时工具/资源/回填以**明确错误**收敛——不伪装成
/// 实例缺失、空结果或业务失败。
#[tokio::test]
async fn unbound_slot_reports_explicit_runtime_errors() {
    let pair = test_fixture::connect(WorkflowMcpServer::new(WorkflowInstanceInput::slot())).await;
    let peer = pair.peer();

    // 工具面：实例可见、参数合法，但运行时未就绪 ⇒ 工具级失败文本。
    let workflow = test_fixture::call_tool(&peer, "Workflow", json!({})).await;
    assert_eq!(workflow.is_error, Some(true), "未绑定时 Workflow 必须失败");
    assert!(
        test_fixture::first_text(&workflow).contains("not bound"),
        "{}",
        test_fixture::first_text(&workflow)
    );

    let status = test_fixture::call_tool(&peer, "WorkflowStatus", json!({})).await;
    assert_eq!(
        status.is_error,
        Some(true),
        "未绑定时 WorkflowStatus 必须失败"
    );
    assert!(
        test_fixture::first_text(&status).contains("not bound"),
        "{}",
        test_fixture::first_text(&status)
    );

    // 参数违约仍是协议级错误（先于槽位解析，N3 分层不因未绑定而改变）。
    let typed = test_fixture::call_tool_protocol_error(
        &peer,
        "WorkflowStatus",
        json!({"limit": "5"}),
        None,
    )
    .await;
    test_fixture::assert_invalid_params(&typed, "未绑定 + limit 类型错误");

    // 回填面：没有可校验的消费者绑定 ⇒ 协议级拒绝。
    let result = test_fixture::call_tool_protocol_error(
        &peer,
        "WorkflowAgentResult",
        json!({"runId": "run-1", "taskId": "task-1", "result": {"kind": "skipped"}}),
        None,
    )
    .await;
    test_fixture::assert_invalid_params(&result, "未绑定槽位的回填");

    // 资源面：不伪造空结果（内部错误，明确「尚未绑定」）。
    let error = test_fixture::read_resource(&peer, "workflow://runs")
        .await
        .expect_err("未绑定时资源读取必须失败");
    assert!(error.contains("not bound"), "{error}");

    pair.shutdown().await;
}

/// F2（对抗审查）：生产存在「先订阅、后绑定」窗口——`activate()` 之后池初始化即建立
/// `subscriptions/listen`（`peri-middlewares/src/mcp/initialize.rs` 的
/// `setup_subscription`），而 builtin `workflow` 实例的运行时槽位要到会话发布前才绑定
/// （`session_lifecycle.rs` 的 bind 点）。SDK 在 handler 执行**之前**就发出
/// acknowledge，因此客户端此时看到的订阅「连接正常」；订阅若在绑定前放弃注册唤醒，
/// 之后绑定也不会重建 receiver ⇒ 通知永远不来（静默失效）。
///
/// 契约：绑定前的订阅必须保持可唤醒，绑定后发布的任务更新必须到达。
#[tokio::test]
async fn subscription_listening_before_bind_is_woken_after_binding() {
    let slot = WorkflowInstanceInput::slot();
    let pair = test_fixture::connect(WorkflowMcpServer::new(slot.clone())).await;
    let peer = pair.peer();

    // ① 未绑定即 listen（生产时序：池初始化先于会话就绪）。
    let mut subscription = peer
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions([crate::resources::TASKS_URI])
                .build(),
        )
        .await
        .expect("绑定前必须能建立订阅（acknowledge 先于 handler）");

    // ② 会话就绪：绑定运行时（生产 bind 点）。
    let runtime = test_fixture::make_runtime();
    assert_eq!(
        slot.bind(Arc::clone(&runtime)),
        BindOutcome::Bound,
        "空槽首次绑定必须成功"
    );

    // ③ 绑定后发布任务：先可见后通知。绑定等待存在固有时序（通知只承担唤醒信号，
    //    不承担可靠投递），因此循环发布 + 等待，直到收到该 URI 的 updated 通知。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut seq = 0u64;
    let uri = loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "绑定后订阅仍未收到唤醒信号：订阅在未绑定窗口静默失效（F2）"
        );
        let outcome = runtime
            .tasks()
            .publish(format!("task-{seq}"), test_fixture::task_params("run-1"));
        assert!(
            matches!(outcome, PublishOutcome::Published(_)),
            "任务发布必须提交"
        );
        seq += 1;
        match tokio::time::timeout(Duration::from_millis(100), subscription.next()).await {
            Ok(Ok(Some(ServerNotification::ResourceUpdatedNotification(update)))) => {
                break update.params.uri;
            }
            Ok(Ok(Some(other))) => panic!("收到与订阅无关的通知：{other:?}"),
            Ok(Err(error)) => panic!("订阅流读取失败：{error}"),
            Ok(Ok(None)) => panic!("订阅流提前结束（end = {:?}）", subscription.end()),
            // 本轮未收到：继续发布，覆盖「发布早于 receiver 注册」的竞态窗口。
            Err(_) => {}
        }
    };
    assert_eq!(
        uri,
        crate::resources::TASKS_URI,
        "唤醒 URI 必须是订阅的规范 URI"
    );

    let _ = subscription.cancel().await;
    pair.shutdown().await;
}

#[tokio::test]
async fn wire_consumer_commands_require_token_and_claim_is_retry_safe() {
    let runtime = test_fixture::make_runtime();
    let token = runtime.bind_consumer_token();
    let _ = runtime
        .tasks()
        .publish("task-a".into(), test_fixture::task_params("run-a"));
    let pair = test_fixture::connect(test_fixture::server(runtime.clone())).await;
    let peer = pair.peer();
    let claim = json!({"runId":"run-a","taskId":"task-a","claimId":"request-a"});
    for (name, args) in [
        ("WorkflowAgentClaim", claim.clone()),
        ("WorkflowTaskObserved", json!({"runId":"run-a"})),
        ("WorkflowConsumerReady", json!({"ready":true})),
        ("WorkflowCompletionAck", json!({"runId":"run-a"})),
        (
            "WorkflowToolResult",
            json!({
                "runId":"run-a","taskId":"tool-a",
                "outcome":{"kind":"ok","content":"done"}
            }),
        ),
        ("WorkflowClose", json!({})),
    ] {
        test_fixture::assert_invalid_params(
            &test_fixture::call_tool_protocol_error(&peer, name, args.clone(), None).await,
            name,
        );
        test_fixture::assert_invalid_params(
            &test_fixture::call_tool_protocol_error(
                &peer,
                name,
                args,
                Some(test_fixture::consumer_meta("wrong")),
            )
            .await,
            name,
        );
    }
    let first = test_fixture::call_tool_with_meta(
        &peer,
        "WorkflowAgentClaim",
        claim.clone(),
        Some(test_fixture::consumer_meta(&token)),
    )
    .await;
    let retry = test_fixture::call_tool_with_meta(
        &peer,
        "WorkflowAgentClaim",
        claim,
        Some(test_fixture::consumer_meta(&token)),
    )
    .await;
    assert_eq!(
        test_fixture::first_text(&first),
        test_fixture::first_text(&retry)
    );
    assert_eq!(
        serde_json::from_str::<Value>(&test_fixture::first_text(&first)).unwrap()["taskId"],
        "task-a"
    );
    let other = test_fixture::call_tool_with_meta(
        &peer,
        "WorkflowAgentClaim",
        json!({"runId":"run-a","taskId":"task-a","claimId":"another"}),
        Some(test_fixture::consumer_meta(&token)),
    )
    .await;
    assert_eq!(test_fixture::first_text(&other), "null");
    test_fixture::call_tool_with_meta(
        &peer,
        "WorkflowConsumerReady",
        json!({"ready":false}),
        Some(test_fixture::consumer_meta(&token)),
    )
    .await;
    assert_eq!(
        runtime.tasks().get("run-a", "task-a").unwrap().state,
        WorkflowTaskState::Cancelled
    );
    pair.shutdown().await;
}

/// 断开（`WorkflowConsumerReady{ready:false}`）放弃一个已 claim 的 tool 任务：
/// 终态仍是 `cancelled`，但「MCP 调用可能已 dispatch」必须留下 run 级未知副作用
/// （与 run kill / 单 agent kill 路径同口径）。
#[tokio::test]
async fn disconnect_settles_executing_tool_task_and_marks_unknown_side_effects() {
    let runtime = test_fixture::make_runtime();
    let _ = runtime.tasks().publish_tool(
        "tool-a".into(),
        "run-a",
        "Read".into(),
        Default::default(),
        0,
        0,
    );
    assert!(matches!(
        runtime
            .tasks()
            .mark_executing("run-a", "tool-a", Default::default()),
        ClaimOutcome::Claimed(_)
    ));
    assert!(
        !runtime.tasks().run_unknown_side_effects("run-a"),
        "claim 本身不产生未知副作用"
    );
    let token = runtime.bind_consumer_token();
    let pair = test_fixture::connect(test_fixture::server(Arc::clone(&runtime))).await;
    let peer = pair.peer();

    let ready = test_fixture::call_tool_with_meta(
        &peer,
        "WorkflowConsumerReady",
        json!({"ready":false}),
        Some(test_fixture::consumer_meta(&token)),
    )
    .await;
    assert_ne!(
        ready.is_error,
        Some(true),
        "{}",
        test_fixture::first_text(&ready)
    );

    assert_eq!(
        runtime.tasks().get("run-a", "tool-a").unwrap().state,
        WorkflowTaskState::Cancelled
    );
    assert!(
        runtime.tasks().run_unknown_side_effects("run-a"),
        "放弃执行中的 tool 调用必须标记未知副作用"
    );
    pair.shutdown().await;
}
