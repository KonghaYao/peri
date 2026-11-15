//! 资源读/订阅与通知透传的 wire 验收。

use super::*;

// ─── 资源 / 订阅 / 通知 ──────────────────────────────────────────────────────

#[tokio::test]
#[allow(deprecated)]
async fn resources_and_subscriptions_preserve_uris_and_forward_notifications() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;
    let peer = downstream.client.peer();

    let resources = peer.list_resources(None).await.expect("resources/list");
    assert_eq!(resources.resources[0].uri, URI);

    let read = peer
        .read_resource_once(ReadResourceRequestParams::new(URI))
        .await
        .expect("resources/read");
    let ReadResourceResponse::Complete(read) = read else {
        panic!("read must complete");
    };
    let ResourceContents::TextResourceContents { text, uri, .. } = &read.contents[0] else {
        panic!("text resource expected");
    };
    assert_eq!(uri, URI);
    assert_eq!(text, &format!("content:{URI}"));

    peer.subscribe(SubscribeRequestParams::new(URI))
        .await
        .expect("resources/subscribe");
    peer.subscribe(SubscribeRequestParams::new(URI))
        .await
        .expect("duplicate subscribe");
    assert_eq!(
        mock.subscriptions(),
        vec![URI.to_owned()],
        "引用计数只上行一次"
    );

    wait_until(|| {
        downstream
            .recorder
            .resource_updates()
            .contains(&URI.to_owned())
    })
    .await;
    assert_eq!(downstream.recorder.resource_updates(), vec![URI.to_owned()]);

    peer.unsubscribe(UnsubscribeRequestParams::new(URI))
        .await
        .expect("unsubscribe once");
    assert!(mock.unsubscriptions().is_empty(), "仍有引用时不得退订");
    peer.unsubscribe(UnsubscribeRequestParams::new(URI))
        .await
        .expect("unsubscribe twice");
    assert_eq!(mock.unsubscriptions(), vec![URI.to_owned()]);

    stop_server(upstream.server).await;
}

#[tokio::test]
async fn list_changed_logging_and_custom_notifications_are_forwarded() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;

    downstream
        .client
        .peer()
        .call_tool_once(CallToolRequestParams::new("notify"))
        .await
        .expect("notify tool");

    wait_until(|| {
        downstream.recorder.tool_list_changed() == 1
            && downstream.recorder.logging().len() == 1
            && downstream.recorder.custom() == vec!["workspace/custom".to_owned()]
    })
    .await;
    assert!(downstream.recorder.logging()[0].contains("hello"));

    stop_server(upstream.server).await;
}

#[tokio::test]
async fn list_and_custom_requests_preserve_incoming_meta() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;
    let peer = downstream.client.peer();

    let mut params = PaginatedRequestParams::default();
    let mut meta = RequestMetaObject::new();
    meta.0
         .0
        .insert("trace".to_owned(), serde_json::json!("list"));
    params.meta = Some(meta);
    peer.list_tools(Some(params)).await.expect("tools/list");
    let forwarded = mock.list_tool_meta().expect("list meta recorded");
    assert_eq!(
        forwarded
            .0
             .0
            .get("trace")
            .and_then(serde_json::Value::as_str),
        Some("list"),
        "列表请求的 _meta 必须转发到上游"
    );

    let mut request =
        CustomRequest::new("workspace/probe", Some(serde_json::json!({ "answer": 42 })));
    request
        .get_meta_mut()
        .0
         .0
        .insert("trace".to_owned(), serde_json::json!("custom"));
    let result = peer
        .send_request(ClientRequest::CustomRequest(request))
        .await
        .expect("custom request");
    let ServerResult::CustomResult(result) = result else {
        panic!("custom result expected");
    };
    assert_eq!(result.0, serde_json::json!({ "method": "workspace/probe" }));

    let custom = mock.custom_requests();
    assert_eq!(custom.len(), 1);
    assert_eq!(custom[0].0, "workspace/probe");
    assert_eq!(
        custom[0]
            .1
             .0
             .0
            .get("trace")
            .and_then(serde_json::Value::as_str),
        Some("custom"),
        "custom 请求的 _meta 必须转发到上游"
    );

    stop_server(upstream.server).await;
}

#[tokio::test]
async fn capability_snapshot_omits_unforwarded_surfaces() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    let upstream = connect_auto_upstream(&proxy, mock).await;
    let downstream = connect_downstream(&proxy, true).await;

    let info = downstream
        .client
        .peer()
        .peer_info()
        .expect("modern peer info");
    assert!(
        info.capabilities.prompts.is_none(),
        "prompts 未转发不得声明"
    );
    assert!(
        info.capabilities.skills().is_none(),
        "skills 未转发不得声明"
    );
    assert!(info.capabilities.supports_tasks(), "tasks 必须保留");
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.resources.is_some());

    stop_server(upstream.server).await;
}

#[tokio::test]
#[allow(deprecated)]
async fn concurrent_first_subscribe_establishes_upstream_once() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    mock.delay_subscribes(100);
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;
    let peer = downstream.client.peer().clone();

    let first = tokio::spawn({
        let peer = peer.clone();
        async move { peer.subscribe(SubscribeRequestParams::new(URI)).await }
    });
    let second = tokio::spawn({
        let peer = peer.clone();
        async move { peer.subscribe(SubscribeRequestParams::new(URI)).await }
    });
    first.await.expect("first join").expect("first subscribe");
    second
        .await
        .expect("second join")
        .expect("second subscribe waits for establishment");
    assert_eq!(
        mock.subscriptions(),
        vec![URI.to_owned()],
        "并发首订阅只允许一次上行"
    );

    stop_server(upstream.server).await;
}

#[tokio::test]
#[allow(deprecated)]
async fn failed_subscribe_and_final_unsubscribe_are_retryable() {
    let proxy = UpstreamProxy::new("workspace");
    let mock = MockUpstream::new();
    mock.fail_next_subscribes(1);
    let upstream = connect_legacy_upstream(&proxy, mock.clone()).await;
    let downstream = connect_downstream(&proxy, false).await;
    let peer = downstream.client.peer();

    peer.subscribe(SubscribeRequestParams::new(URI))
        .await
        .expect_err("first subscribe fails upstream");
    assert!(mock.subscriptions().is_empty());
    peer.subscribe(SubscribeRequestParams::new(URI))
        .await
        .expect("retry establishes subscription");
    assert_eq!(mock.subscriptions(), vec![URI.to_owned()]);

    mock.fail_next_unsubscribes(1);
    peer.unsubscribe(UnsubscribeRequestParams::new(URI))
        .await
        .expect_err("first unsubscribe fails upstream");
    assert!(mock.unsubscriptions().is_empty(), "失败退订不得提交引用");
    peer.unsubscribe(UnsubscribeRequestParams::new(URI))
        .await
        .expect("retry reaches upstream");
    assert_eq!(mock.unsubscriptions(), vec![URI.to_owned()]);

    stop_server(upstream.server).await;
}
