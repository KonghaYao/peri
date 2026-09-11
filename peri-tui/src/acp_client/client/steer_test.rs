use super::*;
use peri_acp::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage};
use serde_json::json;

fn make_snapshot() -> serde_json::Value {
    json!({"sessionId":"s","generation":"g","revision":1,"items":[]})
}

#[tokio::test]
async fn test_user_input_capability_requires_explicit_server_support() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, _) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let server = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, .. } = server_transport.recv().await.unwrap()
        else {
            panic!("应收到 initialize");
        };
        assert_eq!(method, "initialize", "应先协商能力");
        server_transport
            .send_response(
                id,
                Ok(json!({"agentCapabilities":{"_meta":{"peri.userInputQueue":true}}})),
            )
            .await
            .unwrap();
    });
    client.register_ui_commands(&[]).await.unwrap();
    assert!(
        client.supports_user_input_queue(),
        "只有服务端明确支持才启用新队列"
    );
    server.await.unwrap();
    client.close();
}

#[tokio::test]
async fn test_user_input_old_server_keeps_legacy_path() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, _) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let server = tokio::spawn(async move {
        let IncomingMessage::Request { id, .. } = server_transport.recv().await.unwrap() else {
            panic!("应收到 initialize");
        };
        server_transport
            .send_response(id, Ok(json!({"agentCapabilities":{}})))
            .await
            .unwrap();
    });
    client.register_ui_commands(&[]).await.unwrap();
    assert!(
        !client.supports_user_input_queue(),
        "缺少能力不能猜测支持或双发"
    );
    server.await.unwrap();
    client.close();
}

#[tokio::test]
async fn test_user_input_enqueue_preserves_wire_identity_and_raw_draft() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, _) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s", false);
    client.spawn_pump(notification_tx);
    let server = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, params } =
            server_transport.recv().await.unwrap()
        else {
            panic!("应收到 enqueue");
        };
        assert_eq!(method, "session/input/enqueue", "输入必须走专用准入");
        assert_eq!(params["generation"], "g", "必须绑定服务端实例");
        assert_eq!(params["commandId"], "c", "重试使用稳定命令身份");
        assert_eq!(params["inputId"], "i", "输入身份原样透传");
        assert_eq!(
            params["originalDraft"], "  中文\n@image /tmp/a.png\n",
            "原稿不能 trim"
        );
        server_transport.send_response(id, Ok(json!({"snapshot":make_snapshot(),"results":[{"inputId":"i","state":"queued"}]}))).await.unwrap();
    });
    let receipt = client
        .enqueue_user_input(&EnqueueUserInputRequest {
            session_id: "s".into(),
            generation: "g".into(),
            command_id: "c".into(),
            input_id: "i".into(),
            content: peri_acp_types::messages::MessageContent::text("  中文\n@image /tmp/a.png\n"),
            original_draft: "  中文\n@image /tmp/a.png\n".into(),
        })
        .await
        .unwrap();
    assert_eq!(receipt.results[0].input_id, "i", "回执应保留输入身份");
    server.await.unwrap();
    client.close();
}

#[tokio::test]
async fn test_user_input_control_does_not_wait_for_long_prompt() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, _) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s", false);
    client.spawn_pump(notification_tx);
    let prompt_client = client.clone();
    let prompt = tokio::spawn(async move {
        prompt_client
            .prompt(
                &peri_acp_types::messages::MessageContent::text("work"),
                Some("run".into()),
            )
            .await
    });
    let IncomingMessage::Request {
        id: prompt_id,
        method,
        ..
    } = server_transport.recv().await.unwrap()
    else {
        panic!("应收到 prompt");
    };
    assert_eq!(method, "session/prompt", "先保持现有 prompt 在途");
    let server = tokio::spawn(async move {
        let IncomingMessage::Request { id, method, params } =
            server_transport.recv().await.unwrap()
        else {
            panic!("应收到 dispatch");
        };
        assert_eq!(
            method, "session/input/dispatch",
            "在途 prompt 不应阻塞控制请求"
        );
        assert_eq!(params["inputIds"], json!(["b"]), "只发送选择的 B");
        server_transport
            .send_response(id, Ok(json!({"snapshot":make_snapshot(),"results":[]})))
            .await
            .unwrap();
        server_transport
            .send_response(prompt_id, Ok(json!({})))
            .await
            .unwrap();
    });
    let request = DispatchUserInputsRequest {
        session_id: "s".into(),
        generation: "g".into(),
        command_id: "c".into(),
        input_ids: vec!["b".into()],
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.dispatch_user_inputs(&request),
    )
    .await
    .expect("控制请求应在 prompt 完成前响应")
    .unwrap();
    prompt.await.unwrap().unwrap();
    server.await.unwrap();
    client.close();
}

#[tokio::test]
async fn test_user_input_command_does_not_migrate_to_new_session() {
    let (client_transport, _server_transport) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("new", false);
    let error = client.user_input_snapshot("old").await.unwrap_err();
    assert_eq!(error.code, -32602, "会话切换后旧命令必须明确拒绝");
    client.close();
}

#[tokio::test]
async fn test_user_input_load_binds_snapshot_before_live_delivery() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, mut notifications) = AcpTuiClient::new(client_transport);
    client.user_input_queue.store(true, Ordering::Release);
    client.spawn_pump(notification_tx);
    let loader = client.clone();
    let loading = tokio::spawn(async move { loader.load_session("s", "/tmp", None).await });
    let IncomingMessage::Request { id, method, .. } = server_transport.recv().await.unwrap() else {
        panic!("应收到 load");
    };
    assert_eq!(method, "session/load", "先加载同一会话");
    server_transport
        .send_response(id, Ok(json!({})))
        .await
        .unwrap();
    let IncomingMessage::Request {
        id: snapshot_id,
        method,
        ..
    } = server_transport.recv().await.unwrap()
    else {
        panic!("load 必须在释放边界前查询实例");
    };
    assert_eq!(
        method, "session/input/snapshot",
        "能力已协商时必须绑定服务端实例"
    );
    server_transport
        .send_notification(
            "peri/agent_event",
            json!({
                "sessionId":"s", "event_json":serde_json::to_string(&AcpEvent::UserInputDelivered {
                    generation:"g".into(), input_id:"a".into(),
                    content:peri_acp_types::messages::MessageContent::text("queued"),
                }).unwrap()
            }),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), notifications.recv())
            .await
            .is_err(),
        "快照绑定前的实时输入必须留在接收顺序中等待"
    );
    let mut snapshot = make_snapshot();
    snapshot["activeRequestId"] = json!("run");
    server_transport
        .send_response(snapshot_id, Ok(snapshot))
        .await
        .unwrap();
    assert_eq!(
        loading.await.unwrap().unwrap(),
        "s",
        "受信绑定后才完成 load"
    );
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), notifications.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(first, AcpNotification::AgentEvent { event:AcpEvent::UserInputRunStarted { request_id, .. }, .. } if request_id == "run"),
        "必须先恢复运行 owner，再允许实时消息通过"
    );
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), notifications.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(second, AcpNotification::AgentEvent { event:AcpEvent::UserInputDelivered { input_id, .. }, .. } if input_id == "a"),
        "绑定期间到达的 canonical 消息不能丢失"
    );
    client.close();
}

#[tokio::test]
async fn test_user_input_stop_preserves_managed_run_identity_on_wire() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s", false);
    client.user_input_queue.store(true, Ordering::Release);
    let (_, local_generation) = client.lifecycle.stable_identity().unwrap();
    assert!(
        client
            .lifecycle
            .bind_user_input_generation("s", local_generation, "g"),
        "应绑定实例"
    );
    assert!(
        client
            .lifecycle
            .open_user_input_run("s", "g", "run")
            .is_some(),
        "应打开实际执行标记"
    );
    client.cancel().await.unwrap();
    let IncomingMessage::Notification { method, params } = server_transport.recv().await.unwrap()
    else {
        panic!("应收到 cancel 通知");
    };
    assert_eq!(method, "session/cancel", "沿现有取消协议发送");
    assert_eq!(
        params,
        json!({"sessionId":"s","generation":"g","requestId":"run"}),
        "Stop 必须在退役本地 marker 之前保存目标身份，避免取消后续 run"
    );
    assert!(
        client.lifecycle.active_user_input_run().is_none(),
        "Stop 后本地 owner 应退役"
    );
    client.close();
}
