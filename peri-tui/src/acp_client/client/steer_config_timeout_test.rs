use super::*;
use peri_acp::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage};

#[tokio::test(start_paused = true)]
#[serial_test::serial]
async fn steer_config_timeout_releases_gate_and_ignores_late_responses() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let original = crate::kit::atoms::SERVICE_SNAPSHOT.state().read().clone();
    for model in [true, false] {
        let requester = client.clone();
        let request = tokio::spawn(async move {
            if model {
                requester.set_model("late-model").await
            } else {
                requester.set_mode("auto-mode").await
            }
        });
        let Some(IncomingMessage::Request { id, method, .. }) = server.recv().await else {
            panic!("配置请求未发出")
        };
        assert_eq!(
            method,
            if model {
                "session/set_config_option"
            } else {
                "session/set_mode"
            }
        );
        assert!(client.lifecycle.operation_gate().try_lock().is_err());
        tokio::time::advance(CONFIG_REQUEST_TIMEOUT + std::time::Duration::from_millis(1)).await;
        let error = request.await.unwrap().unwrap_err();
        assert!(error.message.contains("timed out"));
        let gate = client.lifecycle.operation_gate();
        let guard = gate.try_lock().expect("超时必须释放 gate");
        drop(guard);
        client.force_stable_for_test("session-b", false);
        server.send_response(id, Ok(json!({}))).await.unwrap();
        tokio::task::yield_now().await;
        assert_eq!(client.stable_session_identity().unwrap().0, "session-b");
        {
            let snapshot = crate::kit::atoms::SERVICE_SNAPSHOT.state();
            let snapshot = snapshot.read();
            assert_eq!(snapshot.permission_mode, original.permission_mode);
            assert_eq!(snapshot.model_alias, original.model_alias);
            assert_eq!(snapshot.model_name, original.model_name);
        }
        // 新请求可以真正穿过 gate，并且不会误认旧响应为它的完成。
        let requester = client.clone();
        let next = tokio::spawn(async move { requester.set_model("current-model").await });
        let Some(IncomingMessage::Request { id, params, .. }) = server.recv().await else {
            panic!("后续请求被 gate 阻塞")
        };
        assert_eq!(params["sessionId"], "session-b");
        assert!(!next.is_finished());
        server.send_response(id, Ok(json!({}))).await.unwrap();
        next.await.unwrap().unwrap();
    }
}
