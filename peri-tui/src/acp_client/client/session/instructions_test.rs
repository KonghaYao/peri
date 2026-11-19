//! 交互式 TUI 会话指令（`_meta["peri.instructions"]`）的注入边界。

use super::AcpTuiClient;

use peri_acp::transport::AcpTransport;
use peri_acp::transport::mpsc::MpscServerTransport;
use peri_acp::transport::types::IncomingMessage;
use peri_acp::transport::types::RequestId;
use serde_json::Value;
use std::time::Duration;

const INSTRUCTIONS_KEY: &str = "peri.instructions";

fn interactive_client() -> (AcpTuiClient, MpscServerTransport) {
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new_interactive(transport);
    (client, server)
}

fn headless_client() -> (AcpTuiClient, MpscServerTransport) {
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    (client, server)
}

async fn next_request(server: &MpscServerTransport) -> (RequestId, String, Value) {
    let message = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .expect("expected a client request")
        .expect("transport must stay open");
    let IncomingMessage::Request { id, method, params } = message else {
        panic!("expected request, got {message:?}")
    };
    (id, method, params)
}

/// 正文本身必须满足服务端准入：非空、≤64 KiB、不含保留的 cache 边界控制字。
#[test]
fn tui_session_instructions_fit_server_admission() {
    let instructions = crate::acp_client::TUI_SESSION_INSTRUCTIONS;
    assert!(!instructions.trim().is_empty(), "指令正文不得为空");
    assert!(
        instructions.len() <= 64 * 1024,
        "指令正文 {} 字节超过服务端 64 KiB 准入上限",
        instructions.len()
    );
    assert!(
        !instructions.contains(peri_acp_types::model::SYSTEM_PROMPT_DYNAMIC_BOUNDARY),
        "指令正文不得含保留的 cache boundary 控制字"
    );
}

/// 交互式 `session/new` 冻结指令，Headless 不注入。
#[tokio::test]
#[serial_test::serial]
async fn interactive_new_session_carries_instructions_and_headless_does_not() {
    crate::kit::atoms::init_atoms();

    let (interactive, interactive_server) = interactive_client();
    let opener = interactive.clone();
    let create = tokio::spawn(async move { opener.new_session("/tmp", None).await });
    let (id, method, params) = next_request(&interactive_server).await;
    assert_eq!(method, "session/new");
    assert_eq!(
        params["_meta"][INSTRUCTIONS_KEY],
        crate::acp_client::TUI_SESSION_INSTRUCTIONS
    );
    interactive_server
        .send_response(id, Ok(serde_json::json!({"sessionId": "interactive"})))
        .await
        .expect("interactive session/new response must be delivered");
    assert_eq!(
        create
            .await
            .expect("interactive new session task must not panic")
            .expect("interactive new session must succeed"),
        "interactive"
    );

    let (headless, headless_server) = headless_client();
    let opener = headless.clone();
    let create = tokio::spawn(async move { opener.new_session("/tmp", None).await });
    let (id, method, params) = next_request(&headless_server).await;
    assert_eq!(method, "session/new");
    assert!(
        params.get("_meta").is_none(),
        "Headless session/new 不得带 _meta：{params}"
    );
    headless_server
        .send_response(id, Ok(serde_json::json!({"sessionId": "headless"})))
        .await
        .expect("headless session/new response must be delivered");
    assert_eq!(
        create
            .await
            .expect("headless new session task must not panic")
            .expect("headless new session must succeed"),
        "headless"
    );
}

/// `session/load` 恢复服务端已冻结的旧值，客户端不补注入。
#[tokio::test]
#[serial_test::serial]
async fn session_load_does_not_carry_instructions() {
    crate::kit::atoms::init_atoms();

    let (client, server) = interactive_client();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("existing", "/tmp", None).await });
    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/load");
    assert!(
        params
            .get("_meta")
            .and_then(|meta| meta.get(INSTRUCTIONS_KEY))
            .is_none(),
        "session/load 不得补注入 peri.instructions：{params}"
    );
    server
        .send_response(id, Ok(serde_json::json!({})))
        .await
        .expect("session/load response must be delivered");
    assert_eq!(
        load.await
            .expect("load session task must not panic")
            .expect("load session must succeed"),
        "existing"
    );
}
