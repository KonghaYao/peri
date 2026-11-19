use super::*;
use peri_acp::transport::{
    AcpTransport,
    mpsc::MpscServerTransport,
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
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client
        .session_workspace
        .store(true, std::sync::atomic::Ordering::Release);
    (client, server)
}

#[tokio::test]
#[serial_test::serial]
async fn legacy_history_context_without_binding_can_restore_saved_directory() {
    let (client, server) = client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("legacy", "/startup", None).await });
    let (id, _) = request(&server, "peri/session_context").await;
    let mut legacy = context("/saved/project");
    legacy["binding"] = Value::Null;
    server.send_response(id, Ok(legacy)).await.expect("legacy history context without binding can restore saved directory: fixture task and request must complete successfully");
    let (id, params) = request(&server, "session/load").await;
    assert_eq!(params["cwd"], "/saved/project");
    server.send_response(id, Ok(json!({}))).await.expect("legacy history context without binding can restore saved directory: fixture task and request must complete successfully");
    assert_eq!(load.await.expect("legacy history context without binding can restore saved directory: the fixture operation must finish without an inner failure").expect("legacy history context without binding can restore saved directory: the fixture operation must finish without an inner failure"), "legacy");
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some("/saved/project")
    );
}

#[tokio::test]
async fn legacy_history_preview_does_not_switch_or_prepare_execution() {
    use peri_acp_types::messages::BaseMessage;
    use peri_acp_types::store::PersistedPayload;
    use peri_acp_types::store::serialize_persisted_payload;
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
    .expect("legacy history preview does not switch or prepare execution: the serialize_persisted_payload fixture operation uses valid inputs");
    server.send_response(id, Ok(json!({"sessionId":"legacy","binding":null,"payloads":[serde_json::from_str::<Value>(&message).expect("legacy history preview does not switch or prepare execution: fixture JSON must match the expected schema")]}))).await.expect("legacy history preview does not switch or prepare execution: fixture JSON must match the expected schema");
    assert_eq!(
        read.await.expect("legacy history preview does not switch or prepare execution: the fixture operation must finish without an inner failure").expect("legacy history preview does not switch or prepare execution: the fixture operation must finish without an inner failure")[0]
            .as_message()
            .expect("legacy history preview does not switch or prepare execution: the fixture operation must finish without an inner failure")
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
        .expect("request: fixture task and request must complete successfully")
        .expect("request: fixture task and request must complete successfully");
    let IncomingMessage::Request { id, method, params } = msg else {
        panic!("expected request")
    };
    assert_eq!(method, expected);
    (id, params)
}

#[tokio::test]
#[serial_test::serial]
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
        .expect("resume uses saved execution directory and commits it after load: fixture task and request must complete successfully");
    let (id, params) = request(&server, "session/close").await;
    assert_eq!(params["sessionId"], "old");
    server.send_response(id, Ok(json!({}))).await.expect("resume uses saved execution directory and commits it after load: fixture task and request must complete successfully");
    let (id, params) = request(&server, "session/load").await;
    assert_eq!(params["cwd"], "/worktrees/feature/src");
    assert_eq!(client.current_execution_cwd(), None);
    server.send_response(id, Ok(json!({}))).await.expect("resume uses saved execution directory and commits it after load: fixture task and request must complete successfully");
    assert_eq!(load.await.expect("resume uses saved execution directory and commits it after load: the fixture operation must finish without an inner failure").expect("resume uses saved execution directory and commits it after load: the fixture operation must finish without an inner failure"), "target");
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some("/worktrees/feature/src")
    );
}

// 旧语义（失败的目标预检让 `ensure_session` 永久失败）与 39ed 修正冲突：
// 39ed 明确要求目标预检失败不污染当前会话；同场景回归见
// `acp_client::client::recovery_test::failed_history_preflight_keeps_current_session_usable`。

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
        .expect("failed startup load leaves no session and does not create a new one: fixture task and request must complete successfully");
    let (id, _) = request(&server, "session/load").await;
    server
        .send_response(
            id,
            Err(AcpError::new(-32001, "session owned by another host")),
        )
        .await
        .expect("failed startup load leaves no session and does not create a new one: fixture task and request must complete successfully");
    assert!(
        load.await
            .expect("failed startup load leaves no session and does not create a new one: the fixture operation must finish without an inner failure")
            .expect_err("failed startup load leaves no session and does not create a new one: the invalid fixture must be rejected")
            .to_string()
            .contains("another host")
    );
    assert_eq!(client.current_session_id(), None);
    assert_eq!(client.current_execution_cwd(), None);
    // 失败目标保留给下一次 ensure：先重试同一会话，失败后仍不静默新建。
    let retry_client = client.clone();
    let retry = tokio::spawn(async move { retry_client.ensure_session("/main", None).await });
    let (id, _) = request(&server, "peri/session_context").await;
    server
        .send_response(id, Ok(context("/worktrees/feature/src")))
        .await
        .expect("failed startup load leaves no session and does not create a new one: fixture task and request must complete successfully");
    let (id, _) = request(&server, "session/load").await;
    server
        .send_response(
            id,
            Err(AcpError::new(-32001, "session owned by another host")),
        )
        .await
        .expect("failed startup load leaves no session and does not create a new one: fixture task and request must complete successfully");
    assert!(
        retry
            .await
            .expect("failed startup load leaves no session and does not create a new one: the fixture operation must finish without an inner failure")
            .expect_err("failed startup load leaves no session and does not create a new one: the invalid fixture must be rejected")
            .to_string()
            .contains("another host")
    );
    assert_eq!(client.current_session_id(), None);
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
        .expect("continue queries exact workspace directory and distinguishes no candidate: fixture task and request must complete successfully");
    let (id, params) = request(&server, "session/list").await;
    let query = &params["_meta"]["peri.sessionWorkspaceV1"];
    assert_eq!(
        query["scope"],
        json!({"kind":"exact_directory","value":{
            "workspace_id":"00000000-0000-0000-0000-000000000002","relative_cwd":"src"
        }})
    );
    assert_eq!(query["limit"], 1);
    server.send_response(id, Ok(json!({"sessions":[],"_meta":{"peri.sessionWorkspaceV1":{"threads":[],"nextCursor":null}}}))).await.expect("continue queries exact workspace directory and distinguishes no candidate: fixture task and request must complete successfully");
    assert_eq!(lookup.await.expect("continue queries exact workspace directory and distinguishes no candidate: the fixture operation must finish without an inner failure").expect("continue queries exact workspace directory and distinguishes no candidate: the fixture operation must finish without an inner failure"), None);
}

#[tokio::test]
#[serial_test::serial]
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
        .expect("continue query failure blocks ensure until explicit new session: fixture task and request must complete successfully");
    let error = lookup.await.expect("continue query failure blocks ensure until explicit new session: fixture task and request must complete successfully").expect_err("continue query failure blocks ensure until explicit new session: the invalid fixture must be rejected");
    client.fail_startup_restore(&error).await;
    assert!(
        client
            .ensure_session("/main", None)
            .await
            .expect_err("continue query failure blocks ensure until explicit new session: the invalid fixture must be rejected")
            .to_string()
            .contains("discovery failed")
    );
    let creator = client.clone();
    let create = tokio::spawn(async move { creator.new_session("/main", None).await });
    let (id, params) = request(&server, "session/new").await;
    assert_eq!(params["cwd"], "/main");
    // Headless 客户端不注入 TUI 专属指令：`_meta` 整体缺席。
    assert!(
        params.get("_meta").is_none(),
        "headless session/new 不得带 _meta"
    );
    server.send_response(id, Ok(json!({"sessionId":"fresh","_meta":{"peri.sessionWorkspaceV1":{"workspace":{"cwd":"/canonical/main"}}}}))).await.expect("continue query failure blocks ensure until explicit new session: fixture task and request must complete successfully");
    assert_eq!(create.await.expect("continue query failure blocks ensure until explicit new session: the fixture operation must finish without an inner failure").expect("continue query failure blocks ensure until explicit new session: the fixture operation must finish without an inner failure"), "fresh");
    assert_eq!(
        client.current_execution_cwd().as_deref(),
        Some("/canonical/main")
    );
    assert_eq!(client.ensure_session("/main", None).await.expect("continue query failure blocks ensure until explicit new session: the fixture operation must finish without an inner failure"), "fresh");
}

#[tokio::test]
async fn unsupported_context_version_cannot_be_used_for_execution() {
    let (client, server) = client();
    let lookup = tokio::spawn(async move { client.session_context(Some("target"), None).await });
    let (id, _) = request(&server, "peri/session_context").await;
    let mut response = context("/worktrees/feature/src");
    response["version"] = json!(2);
    server.send_response(id, Ok(response)).await.expect("unsupported context version cannot be used for execution: fixture task and request must complete successfully");
    assert!(
        lookup
            .await
            .expect("unsupported context version cannot be used for execution: the fixture operation must finish without an inner failure")
            .expect_err("unsupported context version cannot be used for execution: the invalid fixture must be rejected")
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
        .expect("failed input initialization closes loaded session and blocks prompt: fixture task and request must complete successfully");
    let (id, _) = request(&server, "session/load").await;
    server.send_response(id, Ok(json!({}))).await.expect("failed input initialization closes loaded session and blocks prompt: fixture task and request must complete successfully");
    let (id, _) = request(&server, "session/input/snapshot").await;
    server
        .send_response(id, Err(AcpError::new(-32603, "snapshot unavailable")))
        .await
        .expect("failed input initialization closes loaded session and blocks prompt: fixture task and request must complete successfully");
    let (id, params) = request(&server, "session/close").await;
    assert_eq!(params["sessionId"], "target");
    server.send_response(id, Ok(json!({}))).await.expect("failed input initialization closes loaded session and blocks prompt: fixture task and request must complete successfully");
    assert!(
        load.await
            .expect("failed input initialization closes loaded session and blocks prompt: the fixture operation must finish without an inner failure")
            .expect_err("failed input initialization closes loaded session and blocks prompt: the invalid fixture must be rejected")
            .to_string()
            .contains("snapshot unavailable")
    );
    assert_eq!(client.current_execution_cwd(), None);
    assert_eq!(client.current_session_id(), None);
    assert!(
        client
            .ensure_session("/main", None)
            .await
            .expect_err("failed input initialization closes loaded session and blocks prompt: the invalid fixture must be rejected")
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
    server.send_response(id, Ok(json!({}))).await.expect("permission changes address active session not startup shared mode: fixture task and request must complete successfully");
    change.await.expect("permission changes address active session not startup shared mode: fixture task and request must complete successfully").expect("permission changes address active session not startup shared mode: fixture task and request must complete successfully");
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
    server.send_response(id, Ok(json!({}))).await.expect("complete host configuration never targets active workspace: fixture task and request must complete successfully");
    update.await.expect("complete host configuration never targets active workspace: fixture task and request must complete successfully").expect("complete host configuration never targets active workspace: fixture task and request must complete successfully");
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
    server.send_response(id, Ok(json!({}))).await.expect("model changes use host supported session config rpc: fixture task and request must complete successfully");
    task.await.expect("model changes use host supported session config rpc: fixture task and request must complete successfully").expect("model changes use host supported session config rpc: fixture task and request must complete successfully");
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
        server.send_response(id, Ok(json!({}))).await.expect("permission labels translate to acp mode identifiers: fixture task and request must complete successfully");
        task.await.expect("permission labels translate to acp mode identifiers: fixture task and request must complete successfully").expect("permission labels translate to acp mode identifiers: fixture task and request must complete successfully");
    }
}

#[tokio::test]
#[serial_test::serial]
async fn cancelled_load_clears_the_target_published_for_replay() {
    use crate::kit::atoms::{ACTIVE_EXECUTION_CWD, ACTIVE_SESSION_ID};
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
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
        .expect("cancelled load clears the target published for replay: fixture task and request must complete successfully");
    let (_id, _) = request(&server, "session/load").await;
    assert_eq!(&*ACTIVE_SESSION_ID.state().read(), "target");
    load.abort();
    assert!(load.await.expect_err("cancelled load clears the target published for replay: the invalid fixture must be rejected").is_cancelled());
    assert!(ACTIVE_SESSION_ID.state().read().is_empty());
    assert!(ACTIVE_EXECUTION_CWD.state().read().is_none());
    assert!(!client.has_session());
    assert!(client.ensure_session("/startup", None).await.is_err());
}
