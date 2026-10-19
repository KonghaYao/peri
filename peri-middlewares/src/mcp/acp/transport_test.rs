use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::oneshot;

struct Call {
    params: Value,
    reply: oneshot::Sender<Value>,
}

struct FakeGateway {
    calls: mpsc::UnboundedSender<Call>,
    cancelled: mpsc::UnboundedSender<String>,
    active: Arc<AtomicUsize>,
}

struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl AcpMcpGatewayPort for FakeGateway {
    async fn request(
        &self,
        method: &str,
        params: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, AcpMcpError> {
        assert_eq!(method, "mcp/message");
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = Active(self.active.clone());
        let id = params["requestId"].as_str().unwrap().to_owned();
        let (reply, rx) = oneshot::channel();
        self.calls.send(Call { params, reply }).unwrap();
        tokio::select! {
            result = rx => result.map_err(|_| AcpMcpError::unavailable("fake closed")),
            () = cancellation.cancelled() => {
                self.cancelled.send(id).unwrap();
                Err(AcpMcpError { code: -32800, message: "cancelled".into() })
            }
        }
    }
}

fn gateway() -> (
    Arc<FakeGateway>,
    mpsc::UnboundedReceiver<Call>,
    mpsc::UnboundedReceiver<String>,
) {
    let (calls, rx) = mpsc::unbounded_channel();
    let (cancelled, cancel_rx) = mpsc::unbounded_channel();
    (
        Arc::new(FakeGateway {
            calls,
            cancelled,
            active: Default::default(),
        }),
        rx,
        cancel_rx,
    )
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(4), future)
        .await
        .expect("桥接不得悬挂")
}

async fn ping(transport: &mut AcpBridgeTransport, id: i64) {
    transport
        .send(
            serde_json::from_value(json!({"jsonrpc": "2.0", "id": id, "method": "ping"})).unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn cancellation_maps_to_acp_request_and_close_joins_all_forwarders() {
    let (gateway, mut calls, mut cancelled) = gateway();
    let (mut transport, handle) = create_bridge(gateway.clone(), "server".into());
    ping(&mut transport, 1).await;
    let call = bounded(calls.recv()).await.unwrap();
    assert_eq!(call.params["requestId"], "peri-1");
    transport
        .send(
            serde_json::from_value(json!({
                "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bounded(cancelled.recv()).await.as_deref(), Some("peri-1"));
    let error = serde_json::to_value(bounded(transport.receive()).await.unwrap()).unwrap();
    assert_eq!(error["id"], 1);
    assert_eq!(error["error"]["code"], -32800);
    assert!(handle.requests.lock().by_acp.is_empty());

    ping(&mut transport, 2).await;
    let _pending = bounded(calls.recv()).await.unwrap();
    bounded(handle.close()).await;
    assert_eq!(bounded(cancelled.recv()).await.as_deref(), Some("peri-2"));
    let error = serde_json::to_value(bounded(transport.receive()).await.unwrap()).unwrap();
    assert_eq!(error["id"], 2);
    assert!(error.get("error").is_some());
    assert!(bounded(transport.receive()).await.is_none());
    assert_eq!(gateway.active.load(Ordering::SeqCst), 0);
    assert!(handle.runner.lock().await.is_none());
    assert!(handle.requests.lock().by_rmcp.is_empty());
    bounded(handle.close()).await;
}

#[tokio::test]
async fn subscription_ack_reaches_real_rmcp_waiter_and_late_ack_is_rejected() {
    use rmcp::model::{ServerConfig, SubscriptionFilter};
    let (gateway, mut calls, _cancelled) = gateway();
    let (transport, handle) = create_bridge(gateway, "server".into());
    let info = ServerConfig::default()
        .with_protocol_version(serde_json::from_value(json!("2026-07-28")).unwrap());
    let client =
        rmcp::service::serve_directly::<RoleClient, _, _, _, _>((), transport, Some(info.into()));
    // rmcp 首个请求 ID 为 0；先完成 tools/list，订阅请求就是原始 ID 1。
    let (ping_result, ()) = tokio::join!(client.peer().list_all_tools(), async {
        let call = bounded(calls.recv()).await.unwrap();
        call.reply.send(json!({"result": {"tools": []}})).unwrap();
    });
    ping_result.unwrap();
    let filter = SubscriptionFilter::default();
    let mut reply = None;
    let (subscription, ()) = tokio::join!(bounded(client.peer().listen(filter.clone())), async {
        let call = bounded(calls.recv()).await.unwrap();
        assert_eq!(call.params["requestId"], "peri-2");
        assert_eq!(call.params["method"], "subscriptions/listen");
        handle
            .notify(
                "peri-2",
                "notifications/subscriptions/acknowledged",
                Some(json!({
                    "notifications": filter,
                    "_meta": {"io.modelcontextprotocol/subscriptionId": "peri-2"}
                })),
            )
            .unwrap();
        reply = Some(call.reply);
    });
    let mut subscription = subscription.unwrap();
    assert_eq!(subscription.id(), &RequestId::Number(1));
    reply
        .unwrap()
        .send(json!({"result": {
            "resultType": "complete", "_meta": {"io.modelcontextprotocol/subscriptionId": "peri-2"}
        }}))
        .unwrap();
    // 真实 waiter 收到完成响应，证明响应内的订阅 ID 也已经还原。
    assert!(bounded(subscription.next()).await.unwrap().is_none());
    assert!(handle
        .notify(
            "peri-2",
            "notifications/subscriptions/acknowledged",
            Some(json!({
                "notifications": {}, "_meta": {"io.modelcontextprotocol/subscriptionId": "peri-2"}
            }))
        )
        .is_err());
    bounded(handle.close()).await;
}

#[tokio::test]
async fn close_settles_real_rmcp_pending_request_with_error() {
    let (gateway, mut calls, _cancelled) = gateway();
    let (transport, handle) = create_bridge(gateway.clone(), "server".into());
    let client = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        transport,
        Some(rmcp::model::ServerConfig::default().into()),
    );
    let (result, ()) = tokio::join!(bounded(client.peer().list_all_tools()), async {
        let _pending = bounded(calls.recv()).await.unwrap();
        bounded(handle.close()).await;
    });
    assert!(result.is_err());
    assert_eq!(gateway.active.load(Ordering::SeqCst), 0);
    assert!(handle.runner.lock().await.is_none());
}

#[test]
fn response_uses_schema_result_precedence_and_rejects_missing_outcome() {
    assert_eq!(
        parse_message_response(json!({"result": {}, "error": null})).unwrap(),
        json!({})
    );
    assert_eq!(
        parse_message_response(json!({"result": {}, "error": {"code": -1, "message": "ignored"}}))
            .unwrap(),
        json!({})
    );
    assert!(parse_message_response(json!({})).is_err());
    assert_eq!(
        parse_message_response(json!({"result": null})).unwrap(),
        Value::Null
    );
    let error = parse_message_response(
        json!({"error": {"code": -32601, "message": "missing", "data": {"detail": 1}}}),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode(-32601));
    assert_eq!(error.message, "missing");
    assert_eq!(error.data, Some(json!({"detail": 1})));
    assert_eq!(
        parse_message_response(json!({"error": {"code": -1, "message": "null", "data": null}}))
            .unwrap_err()
            .data,
        Some(Value::Null)
    );
}

#[test]
fn associated_ids_translate_without_modifying_application_data() {
    let mut value = json!({"requestId": "peri-2", "_meta": {"io.modelcontextprotocol/subscriptionId": "peri-2"}, "data": {"requestId": "user-data"}});
    translate_ids(&mut value, "peri-2", &RequestId::Number(1)).unwrap();
    assert_eq!(value["requestId"], 1);
    assert_eq!(value["_meta"]["io.modelcontextprotocol/subscriptionId"], 1);
    assert_eq!(value["data"]["requestId"], "user-data");
    assert!(translate_ids(
        &mut json!({"requestId": "other"}),
        "peri-2",
        &RequestId::Number(1)
    )
    .is_err());
}
