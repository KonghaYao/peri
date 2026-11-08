//! Session-load reservations retain prompt ownership until every guard settles.

use super::*;

#[tokio::test]
#[serial]
async fn test_multiple_load_reservations_release_only_after_last_guard() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    let create_client = client.clone();
    let create = tokio::spawn(async move { create_client.ensure_session("/tmp", None).await });
    let (new_id, _) = receive_lifecycle_request(&server_transport, "session/new").await;
    server_transport
        .send_response(new_id, Ok(json!({"sessionId": "old"})))
        .await
        .unwrap();
    create.await.unwrap().unwrap();

    let first = client.reserve_session_load();
    let second = client.reserve_session_load();
    let prompt_client = client.clone();
    let prompt = tokio::spawn(async move {
        prompt_client
            .prompt(
                &peri_acp_types::messages::MessageContent::text("queued"),
                Some("p-queued".into()),
            )
            .await
    });
    drop(first);
    tokio::task::yield_now().await;
    assert!(
        !prompt.is_finished(),
        "one remaining reservation must keep prompt blocked"
    );
    drop(second);

    let (prompt_id, prompt_params) =
        receive_lifecycle_request(&server_transport, "session/prompt").await;
    assert_eq!(prompt_params["sessionId"], "old");
    server_transport
        .send_response(prompt_id, Ok(json!({})))
        .await
        .unwrap();
    prompt.await.unwrap().unwrap();
}

#[tokio::test]
#[serial]
async fn test_failed_load_dispatch_releases_reservation() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    let create_client = client.clone();
    let create = tokio::spawn(async move { create_client.ensure_session("/tmp", None).await });
    let (new_id, _) = receive_lifecycle_request(&server_transport, "session/new").await;
    server_transport
        .send_response(new_id, Ok(json!({"sessionId": "old"})))
        .await
        .unwrap();
    create.await.unwrap().unwrap();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
        crate::kit::thread_load_consumer::ThreadLoadRequest,
    >();
    drop(rx);
    let dispatcher =
        crate::kit::thread_load_consumer::ThreadLoadDispatcher::new(tx, client.clone());
    assert!(dispatcher.send("target".into()).is_err());

    let prompt_client = client.clone();
    let prompt = tokio::spawn(async move {
        prompt_client
            .prompt(
                &peri_acp_types::messages::MessageContent::text("after-send-failure"),
                Some("p-after-failure".into()),
            )
            .await
    });
    let (prompt_id, prompt_params) =
        receive_lifecycle_request(&server_transport, "session/prompt").await;
    assert_eq!(prompt_params["sessionId"], "old");
    server_transport
        .send_response(prompt_id, Ok(json!({})))
        .await
        .unwrap();
    prompt.await.unwrap().unwrap();
}
