use super::*;
use peri_acp::transport::{
    AcpTransport,
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{IncomingMessage, RequestId},
};
use serde_json::Value;
use std::time::Duration;

fn context(cwd: &str) -> Value {
    json!({"version":1,"workspace":{
        "project_id":"00000000-0000-0000-0000-000000000001",
        "workspace_id":"00000000-0000-0000-0000-000000000002",
        "cwd":cwd,"root":"/worktrees/feature","relative_cwd":"src"
    },"binding":{
        "schema_version":1,"revision":1,
        "project_id":"00000000-0000-0000-0000-000000000001",
        "workspace_id":"00000000-0000-0000-0000-000000000002",
        "cwd_relative_to_workspace":"src"
    }})
}

fn client() -> (AcpTuiClient, MpscServerTransport) {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client
        .session_workspace
        .store(true, std::sync::atomic::Ordering::Release);
    (client, server)
}

#[tokio::test]
async fn legacy_history_context_without_binding_can_restore_saved_directory() {
    let (client, server) = client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("legacy", "/startup", None).await });
    let (id, _) = request(&server, "peri/session_context").await;
    let mut legacy = context("/saved/project");
    legacy["binding"] = Value::Null;
    server.send_response(id, Ok(legacy)).await.unwrap();
    let (id, params) = request(&server, "session/load").await;
    assert_eq!(params["cwd"], "/saved/project");
    server.send_response(id, Ok(json!({}))).await.unwrap();
    assert_eq!(load.await.unwrap().unwrap(), "legacy");
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some("/saved/project")
    );
}

#[tokio::test]
async fn legacy_history_preview_does_not_switch_or_prepare_execution() {
    use peri_acp_types::{
        messages::BaseMessage,
        store::{PersistedPayload, serialize_persisted_payload},
    };
    let (client, server) = client();
    client.lifecycle.force_stable("active", false);
    client.project_execution_cwd(Some("/active".into()));
    let reader = client.clone();
    let read = tokio::spawn(async move { reader.read_session_history("legacy").await });
    let (id, params) = request(&server, "peri/session_history").await;
    assert_eq!(params, json!({"sessionId":"legacy"}));
    let message = serialize_persisted_payload(&PersistedPayload::Message(BaseMessage::human(
        "saved message",
    )))
    .unwrap();
    server.send_response(id, Ok(json!({"sessionId":"legacy","binding":null,"payloads":[serde_json::from_str::<Value>(&message).unwrap()]}))).await.unwrap();
    assert_eq!(
        read.await.unwrap().unwrap()[0]
            .as_message()
            .unwrap()
            .content(),
        "saved message"
    );
    assert_eq!(client.current_session_id().as_deref(), Some("active"));
    assert_eq!(client.current_execution_cwd().as_deref(), Some("/active"));
    assert!(client.check_restore_error().is_ok());
}

async fn request(server: &MpscServerTransport, expected: &str) -> (RequestId, Value) {
    let msg = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request { id, method, params } = msg else {
        panic!("expected request")
    };
    assert_eq!(method, expected);
    (id, params)
}

#[tokio::test]
async fn resume_uses_saved_execution_directory_and_commits_it_after_load() {
    let (client, server) = client();
    client.lifecycle.force_stable("old", false);
    client.project_execution_cwd(Some("/main".into()));
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("target", "/main", None).await });
    let (id, params) = request(&server, "peri/session_context").await;
    assert_eq!(params, json!({"sessionId":"target"}));
    assert_eq!(client.current_execution_cwd().as_deref(), Some("/main"));
    server
        .send_response(id, Ok(context("/worktrees/feature/src")))
        .await
        .unwrap();
    let (id, params) = request(&server, "session/close").await;
    assert_eq!(params["sessionId"], "old");
    server.send_response(id, Ok(json!({}))).await.unwrap();
    let (id, params) = request(&server, "session/load").await;
    assert_eq!(params["cwd"], "/worktrees/feature/src");
    assert_eq!(client.current_execution_cwd(), None);
    server.send_response(id, Ok(json!({}))).await.unwrap();
    assert_eq!(load.await.unwrap().unwrap(), "target");
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some("/worktrees/feature/src")
    );
}

#[tokio::test]
async fn unavailable_binding_preserves_old_session_but_blocks_queued_prompt() {
    let (client, server) = client();
    client.lifecycle.force_stable("old", false);
    client.project_execution_cwd(Some("/main".into()));
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("target", "/main", None).await });
    let (id, _) = request(&server, "peri/session_context").await;
    server
        .send_response(id, Err(AcpError::new(-32001, "workspace unavailable")))
        .await
        .unwrap();
    assert!(
        load.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("workspace unavailable")
    );
    assert_eq!(client.current_session_id().as_deref(), Some("old"));
    assert_eq!(client.current_execution_cwd().as_deref(), Some("/main"));
    assert!(
        client
            .ensure_session("/main", None)
            .await
            .unwrap_err()
            .to_string()
            .contains("workspace unavailable")
    );
    let error = client
        .prompt(
            &peri_acp_types::messages::MessageContent::text("do work"),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("workspace unavailable"));
    let error = client
        .dispatch_user_inputs(&peri_acp_types::session::DispatchUserInputsRequest {
            session_id: "old".into(),
            generation: "g".into(),
            command_id: "dispatch".into(),
            input_ids: vec!["input".into()],
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, -32602);
    assert!(error.to_string().contains("workspace unavailable"));
}

#[tokio::test]
async fn failed_startup_load_leaves_no_session_and_does_not_create_a_new_one() {
    let (client, server) = client();
    client.reserve_startup_restore().await;
    let loader = client.clone();
    let load =
        tokio::spawn(async move { loader.load_startup_session("target", "/main", None).await });
    let (id, _) = request(&server, "peri/session_context").await;
    server
        .send_response(id, Ok(context("/worktrees/feature/src")))
        .await
        .unwrap();
    let (id, _) = request(&server, "session/load").await;
    server
        .send_response(
            id,
            Err(AcpError::new(-32001, "session owned by another host")),
        )
        .await
        .unwrap();
    assert!(
        load.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("another host")
    );
    assert_eq!(client.current_session_id(), None);
    assert_eq!(client.current_execution_cwd(), None);
    assert!(
        client
            .ensure_session("/main", None)
            .await
            .unwrap_err()
            .to_string()
            .contains("another host")
    );
}

#[tokio::test]
async fn continue_queries_exact_workspace_directory_and_distinguishes_no_candidate() {
    let (client, server) = client();
    let lookup =
        tokio::spawn(async move { client.latest_thread_in_directory("/launch/src").await });
    let (id, params) = request(&server, "peri/session_context").await;
    assert_eq!(params, json!({"cwd":"/launch/src"}));
    server
        .send_response(id, Ok(context("/worktrees/feature/src")))
        .await
        .unwrap();
    let (id, params) = request(&server, "session/list").await;
    let query = &params["_meta"]["peri.sessionWorkspaceV1"];
    assert_eq!(
        query["scope"],
        json!({"kind":"exact_directory","value":{
            "workspace_id":"00000000-0000-0000-0000-000000000002","relative_cwd":"src"
        }})
    );
    assert_eq!(query["limit"], 1);
    server.send_response(id, Ok(json!({"sessions":[],"_meta":{"peri.sessionWorkspaceV1":{"threads":[],"nextCursor":null}}}))).await.unwrap();
    assert_eq!(lookup.await.unwrap().unwrap(), None);
}

#[tokio::test]
async fn continue_query_failure_blocks_ensure_until_explicit_new_session() {
    let (client, server) = client();
    client.reserve_startup_restore().await;
    let lookup_client = client.clone();
    let lookup =
        tokio::spawn(async move { lookup_client.latest_thread_in_directory("/main").await });
    let (id, _) = request(&server, "peri/session_context").await;
    server
        .send_response(id, Err(AcpError::new(-32001, "discovery failed")))
        .await
        .unwrap();
    let error = lookup.await.unwrap().unwrap_err();
    client.fail_startup_restore(&error).await;
    assert!(
        client
            .ensure_session("/main", None)
            .await
            .unwrap_err()
            .to_string()
            .contains("discovery failed")
    );
    let creator = client.clone();
    let create = tokio::spawn(async move { creator.new_session("/main", None).await });
    let (id, params) = request(&server, "session/new").await;
    assert_eq!(params["cwd"], "/main");
    server.send_response(id, Ok(json!({"sessionId":"fresh","_meta":{"peri.sessionWorkspaceV1":{"workspace":{"cwd":"/canonical/main"}}}}))).await.unwrap();
    assert_eq!(create.await.unwrap().unwrap(), "fresh");
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some("/canonical/main")
    );
    assert_eq!(client.ensure_session("/main", None).await.unwrap(), "fresh");
}

#[tokio::test]
async fn unsupported_context_version_cannot_be_used_for_execution() {
    let (client, server) = client();
    let lookup = tokio::spawn(async move { client.session_context(Some("target"), None).await });
    let (id, _) = request(&server, "peri/session_context").await;
    let mut response = context("/worktrees/feature/src");
    response["version"] = json!(2);
    server.send_response(id, Ok(response)).await.unwrap();
    assert!(
        lookup
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("unsupported session context version")
    );
}

#[tokio::test]
async fn failed_input_initialization_closes_loaded_session_and_blocks_prompt() {
    let (client, server) = client();
    client
        .user_input_queue
        .store(true, std::sync::atomic::Ordering::Release);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("target", "/main", None).await });
    let (id, _) = request(&server, "peri/session_context").await;
    server
        .send_response(id, Ok(context("/worktrees/feature/src")))
        .await
        .unwrap();
    let (id, _) = request(&server, "session/load").await;
    server.send_response(id, Ok(json!({}))).await.unwrap();
    let (id, _) = request(&server, "session/input/snapshot").await;
    server
        .send_response(id, Err(AcpError::new(-32603, "snapshot unavailable")))
        .await
        .unwrap();
    let (id, params) = request(&server, "session/close").await;
    assert_eq!(params["sessionId"], "target");
    server.send_response(id, Ok(json!({}))).await.unwrap();
    assert!(
        load.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("snapshot unavailable")
    );
    assert_eq!(client.current_execution_cwd(), None);
    assert_eq!(client.current_session_id(), None);
    assert!(
        client
            .ensure_session("/main", None)
            .await
            .unwrap_err()
            .to_string()
            .contains("snapshot unavailable")
    );
}

#[tokio::test]
async fn permission_changes_address_active_session_not_startup_shared_mode() {
    let (client, server) = client();
    client.lifecycle.force_stable("worktree-session", false);
    let changer = client.clone();
    let change = tokio::spawn(async move { changer.set_mode("accept-edit").await });
    let (id, params) = request(&server, "session/set_mode").await;
    assert_eq!(
        params,
        json!({"sessionId":"worktree-session","modeId":"accept_edit"})
    );
    server.send_response(id, Ok(json!({}))).await.unwrap();
    change.await.unwrap().unwrap();
}

#[tokio::test]
async fn complete_host_configuration_never_targets_active_workspace() {
    let (client, server) = client();
    client.lifecycle.force_stable("worktree-session", false);
    let update = tokio::spawn(async move {
        client
            .update_config(&crate::config::PeriConfig::default())
            .await
    });
    let (id, params) = request(&server, "session/update_config").await;
    assert!(params.get("sessionId").is_none());
    assert!(params.get("config").is_some());
    server.send_response(id, Ok(json!({}))).await.unwrap();
    update.await.unwrap().unwrap();
}

#[tokio::test]
async fn model_changes_use_host_supported_session_config_rpc() {
    let (client, server) = client();
    client.lifecycle.force_stable("feature-session", false);
    let change = client.clone();
    let task = tokio::spawn(async move { change.set_model("strong").await });
    let (id, params) = request(&server, "session/set_config_option").await;
    assert_eq!(
        params,
        json!({"sessionId":"feature-session", "configId":"model", "value":"strong"})
    );
    server.send_response(id, Ok(json!({}))).await.unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn permission_labels_translate_to_acp_mode_identifiers() {
    let (client, server) = client();
    client.lifecycle.force_stable("feature-session", false);
    for (label, wire) in [("accept-edit", "accept_edit"), ("auto-mode", "auto")] {
        let change = client.clone();
        let task = tokio::spawn(async move { change.set_mode(label).await });
        let (id, params) = request(&server, "session/set_mode").await;
        assert_eq!(
            params,
            json!({"sessionId":"feature-session", "modeId":wire})
        );
        server.send_response(id, Ok(json!({}))).await.unwrap();
        task.await.unwrap().unwrap();
    }
}

#[tokio::test]
#[serial_test::serial]
async fn cancelled_load_clears_the_target_published_for_replay() {
    use crate::kit::atoms::{ACTIVE_EXECUTION_CWD, ACTIVE_SESSION_ID};
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new_interactive(transport);
    client
        .session_workspace
        .store(true, std::sync::atomic::Ordering::Release);
    crate::kit::session_boundary::project_session_boundary(None);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("target", "/startup", None).await });
    let (id, _) = request(&server, "peri/session_context").await;
    server
        .send_response(id, Ok(context("/worktrees/feature/src")))
        .await
        .unwrap();
    let (_id, _) = request(&server, "session/load").await;
    assert_eq!(&*ACTIVE_SESSION_ID.state().read(), "target");
    load.abort();
    assert!(load.await.unwrap_err().is_cancelled());
    assert!(ACTIVE_SESSION_ID.state().read().is_empty());
    assert!(ACTIVE_EXECUTION_CWD.state().read().is_none());
    assert!(!client.has_session());
    assert!(client.ensure_session("/startup", None).await.is_err());
}
