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
