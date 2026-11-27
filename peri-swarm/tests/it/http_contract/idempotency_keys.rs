use super::*;

const KEY: &str = "submit-hello";

async fn submit(router: &Router, session: &str, body: Value) -> (StatusCode, Value) {
    let uri = format!("/v1/agents/sessions/{session}/events");
    let (status, _, value) = send(
        router,
        Method::POST,
        &uri,
        Some(body),
        Some(TOKEN),
        &[("idempotency-key", KEY)],
    )
    .await;
    (status, value)
}

async fn items(router: &Router, session: &str) -> Value {
    let uri = format!("/v1/agents/sessions/{session}/items?order=asc");
    let (status, _, page) = send(router, Method::GET, &uri, None, Some(TOKEN), &[]).await;
    assert_eq!(status, StatusCode::OK);
    page
}

fn message(text: &str) -> Value {
    json!({"events":[{"type":"agent.session.input.message",
        "input":[{"role":"user","content":[{"type":"input_text","text":text}]}]}]})
}

#[tokio::test]
async fn official_message_replays_an_opaque_key_without_new_items_and_scopes_it_to_the_session() {
    let harness = harness();
    let router = &harness.router;
    for session in ["agent-01", "agent-02"] {
        assert_eq!(
            create_session(&harness, json!({"logicalId":session}))
                .await
                .0,
            StatusCode::CREATED
        );
    }
    let body = message("hello");
    let (status, first) = submit(router, "agent-01", body.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(first["disposition"], "inserted");
    assert_eq!(first["accepted"], true);
    let id = first["inputId"].as_str().unwrap();
    assert_eq!(id, "4f126633-b225-54ae-83d3-25fcc0285b6e");
    let before = items(router, "agent-01").await;
    assert_eq!(before["items"].as_array().unwrap().len(), 2);
    assert_eq!(before["items"][0]["payload"]["inputId"], id);
    assert!(!first.to_string().contains(KEY) && !before.to_string().contains(KEY));

    let (status, replay) = submit(router, "agent-01", body.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replay["disposition"], "existing");
    assert_eq!(replay["inputId"], id);
    assert_eq!(items(router, "agent-01").await, before);
    let (status, conflict) = submit(router, "agent-01", message("different")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "input_conflict");
    assert_eq!(conflict["error"]["details"]["inputId"], id);
    assert!(!conflict.to_string().contains(KEY));

    let (status, second) = submit(router, "agent-02", body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(second["disposition"], "inserted");
    let second_id = second["inputId"].as_str().unwrap();
    assert_eq!(
        uuid::Uuid::parse_str(second_id).unwrap().get_version_num(),
        5
    );
    assert_ne!(second_id, id);
}

#[tokio::test]
async fn native_header_only_input_uses_the_same_derived_receipt() {
    let harness = harness();
    let router = &harness.router;
    assert_eq!(
        create_session(&harness, json!({"logicalId":"agent-01"}))
            .await
            .0,
        StatusCode::CREATED
    );
    let (status, inserted) = submit(router, "agent-01", json!({"text":"hello"})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(inserted["disposition"], "inserted");
    assert_eq!(inserted["inputId"], "4f126633-b225-54ae-83d3-25fcc0285b6e");
    let before = items(router, "agent-01").await;
    let (status, replay) = submit(router, "agent-01", message("hello")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replay["disposition"], "existing");
    assert_eq!(replay["inputId"], inserted["inputId"]);
    assert_eq!(items(router, "agent-01").await, before);
}
