//! 错误分类、断线降级、重连隔离与单次超时的 wire 验收。

use super::*;

// ─── 错误 / 降级 / 重连 ──────────────────────────────────────────────────────

#[tokio::test]
async fn upstream_error_classification_is_preserved() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;
    let peer = downstream.client.peer();

    let error = peer
        .call_tool_once(CallToolRequestParams::new("fail"))
        .await
        .expect_err("upstream error must surface");
    match error {
        rmcp::ServiceError::McpError(error) => {
            assert_eq!(error.code, ErrorCode(-32042));
            assert_eq!(error.message, "upstream classified failure");
        }
        other => panic!("expected typed upstream error, got {other:?}"),
    }

    let CallToolResponse::Complete(result) = peer
        .call_tool_once(CallToolRequestParams::new("tool_error"))
        .await
        .expect("tool result")
    else {
        panic!("tool error result must complete");
    };
    assert_eq!(result.is_error, Some(true));
    assert_eq!(text_of(&result), "visible tool error");

    stop_server(upstream.server).await;
}

#[tokio::test]
async fn upstream_disconnect_degrades_only_its_own_capability() {
    let workspace = UpstreamProxy::new("workspace");
    let monitor = UpstreamProxy::new("monitor");
    let workspace_mock = MockUpstream::new();
    let monitor_mock = MockUpstream::new();
    let workspace_upstream = connect_auto_upstream(&workspace, workspace_mock).await;
    let monitor_upstream = connect_auto_upstream(&monitor, monitor_mock.clone()).await;
    let workspace_downstream = connect_downstream(&workspace, true).await;
    let monitor_downstream = connect_downstream(&monitor, true).await;

    workspace_upstream
        .server
        .cancel()
        .await
        .expect("stop workspace upstream");
    wait_until(|| matches!(workspace.health(), CapabilityHealth::Degraded { .. })).await;

    let error = workspace_downstream
        .client
        .peer()
        .call_tool_once(CallToolRequestParams::new("echo"))
        .await
        .expect_err("degraded capability must fail");
    assert!(matches!(
        error,
        rmcp::ServiceError::McpError(error) if error.code == ErrorCode::INTERNAL_ERROR
    ));
    assert!(
        workspace.get_info().capabilities.tools.is_none(),
        "降级后 capability 快照不得再声明 tools"
    );

    assert!(monitor.is_ready());
    let monitor_result = monitor_downstream
        .client
        .peer()
        .call_tool_once(CallToolRequestParams::new("echo"))
        .await
        .expect("other capability stays usable");
    assert!(matches!(monitor_result, CallToolResponse::Complete(_)));

    stop_server(monitor_upstream.server).await;
}

#[tokio::test]
async fn reconnect_clears_old_tasks_and_rejects_stale_generations() {
    let proxy = UpstreamProxy::new("workspace");
    let first_mock = MockUpstream::new();
    let first_upstream = connect_auto_upstream(&proxy, first_mock).await;
    let first_generation = first_upstream.generation;
    let first_downstream = connect_downstream(&proxy, true).await;

    let CallToolResponse::Task(first_task) = first_downstream
        .client
        .peer()
        .call_tool_once(CallToolRequestParams::new("task"))
        .await
        .expect("first task")
    else {
        panic!("task expected");
    };
    let old_task_id = first_task.task.task_id;

    stop_server(first_upstream.server).await;
    wait_until(|| matches!(proxy.health(), CapabilityHealth::Degraded { .. })).await;

    let second_mock = MockUpstream::new();
    let second_upstream = connect_auto_upstream(&proxy, second_mock.clone()).await;
    wait_until(|| proxy.is_ready()).await;

    let stale = first_downstream
        .client
        .peer()
        .get_task(GetTaskParams::new(&old_task_id))
        .await
        .expect_err("old task id must not reach the new upstream");
    assert!(matches!(
        stale,
        rmcp::ServiceError::McpError(error) if error.code == ErrorCode::INVALID_PARAMS
    ));
    assert!(second_mock.task_gets().is_empty());

    let CallToolResponse::Task(second_task) = first_downstream
        .client
        .peer()
        .call_tool_once(CallToolRequestParams::new("task"))
        .await
        .expect("second task")
    else {
        panic!("task expected");
    };
    assert_ne!(
        second_task.task.task_id, old_task_id,
        "重连后同一上游 task id 必须分配新的下游 id"
    );

    proxy.mark_lost(first_generation, UpstreamLossReason::TransportClosed);
    assert!(proxy.is_ready(), "旧代次的丢失事件不得降级新连接");

    stop_server(second_upstream.server).await;
}

// ─── modern subscriptions/listen / timeout ───────────────────────────────────

#[tokio::test]
async fn modern_listen_translates_task_ids_and_forwards_resource_updates() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let upstream = connect_auto_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, true).await;

    let CallToolResponse::Task(created) = downstream
        .client
        .peer()
        .call_tool_once(CallToolRequestParams::new("task"))
        .await
        .expect("task call")
    else {
        panic!("task expected");
    };
    let downstream_task_id = created.task.task_id;

    let mut subscription = downstream
        .client
        .peer()
        .listen(
            SubscriptionFilter::builder()
                .resource_subscriptions([URI])
                .task_id(&downstream_task_id)
                .build(),
        )
        .await
        .expect("subscriptions/listen");

    wait_until(|| !mock.listens().is_empty()).await;
    let upstream_filter = mock.listens()[0].clone();
    assert_eq!(
        upstream_filter.resource_subscriptions,
        Some(vec![URI.to_owned()])
    );
    assert_eq!(
        upstream_filter.task_ids,
        Some(vec!["up-task-1".to_owned()]),
        "listen 过滤器中的 task id 必须翻译为上游 id"
    );

    let notification = tokio::time::timeout(TEST_TIMEOUT, subscription.next())
        .await
        .expect("subscription notification before deadline")
        .expect("subscription stream")
        .expect("notification");
    match notification {
        ServerNotification::ResourceUpdatedNotification(update) => {
            assert_eq!(update.params.uri, URI);
        }
        other => panic!("expected resource update, got {other:?}"),
    }

    subscription.cancel().await.expect("cancel subscription");
    stop_server(upstream.server).await;
}

#[tokio::test]
async fn request_timeout_cancels_upstream_without_degrading() {
    let proxy = UpstreamProxy::new("workspace").with_request_timeout(Duration::from_millis(150));
    let mock = MockUpstream::new();
    let _started = mock.held_request_channel();
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;

    let error = downstream
        .client
        .peer()
        .call_tool_once(CallToolRequestParams::new("hold"))
        .await
        .expect_err("slow upstream must time out");
    assert!(matches!(
        error,
        rmcp::ServiceError::McpError(error)
            if error.code == ErrorCode::INTERNAL_ERROR
                && error.message.contains("timed out")
    ));

    wait_until(|| !mock.cancels().is_empty()).await;
    assert_eq!(
        mock.cancels()[0].reason.as_deref(),
        Some("upstream request timed out")
    );
    assert!(proxy.is_ready(), "单次超时不是断线");

    mock.release_held_call();
    stop_server(upstream.server).await;
}
