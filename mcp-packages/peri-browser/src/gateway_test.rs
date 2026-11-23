use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use super::*;
use crate::config::BrowserConfig;

const TEST_CDP_URL: &str = "ws://127.0.0.1:9";

type GatewayStream = BufReader<tokio::net::UnixStream>;

fn config(sock: Option<PathBuf>) -> BrowserConfig {
    BrowserConfig {
        cdp_url: Some(TEST_CDP_URL.to_owned()),
        owner: "peri-browser-test".to_owned(),
        session_name: "test-session".to_owned(),
        gateway_sock: sock,
        provision_lock: std::env::temp_dir().join("peri-browser-gateway-test.lock"),
        provision_lock_wait: Duration::from_millis(100),
        lock_stale_after: Duration::from_secs(1),
        connect_timeout: Duration::from_millis(100),
        page_wait: Duration::from_millis(100),
        idle_yield: Duration::from_millis(50),
        reclaim_wait: Duration::from_millis(100),
        yield_timeout: Duration::from_millis(100),
        request_human_timeout: Duration::from_millis(100),
        http: None,
    }
}

fn page(target_id: &str) -> PageRef {
    PageRef {
        target_id: target_id.to_owned(),
        url: format!("https://example.test/{target_id}"),
        title: target_id.to_owned(),
    }
}

#[test]
fn outgoing_frames_use_the_gateway_tag_and_camel_case_fields() {
    let register = serde_json::to_value(Outgoing::Register {
        session_id: "session-1".to_owned(),
        name: "test-session".to_owned(),
        cdp_url: TEST_CDP_URL.to_owned(),
        pages: vec![page("target-1")],
        state: AgentState::Driving,
    })
    .expect("register serializes");
    assert_eq!(register["type"], "register");
    assert_eq!(register["sessionId"], "session-1");
    assert_eq!(register["cdpUrl"], TEST_CDP_URL);
    assert_eq!(register["state"], "driving");
    assert_eq!(register["pages"][0]["targetId"], "target-1");

    let pages = serde_json::to_value(Outgoing::UpdatePages {
        session_id: "session-1".to_owned(),
        pages: vec![page("target-1"), page("target-2")],
    })
    .expect("pages serialize");
    assert_eq!(pages["type"], "update_pages");
    assert_eq!(pages["pages"].as_array().map(Vec::len), Some(2));

    let state = serde_json::to_value(Outgoing::State {
        session_id: "session-1".to_owned(),
        state: AgentState::WaitingForHuman,
    })
    .expect("state serializes");
    assert_eq!(state["type"], "state");
    assert_eq!(state["state"], "waiting_for_human");

    let human = serde_json::to_value(Outgoing::HumanRequest {
        session_id: "session-1".to_owned(),
        request_id: "request-1".to_owned(),
        page_id: "target-1".to_owned(),
        prompt: "log in".to_owned(),
        timeout_ms: 1_500,
    })
    .expect("human request serializes");
    assert_eq!(human["type"], "human_request");
    assert_eq!(human["requestId"], "request-1");
    assert_eq!(human["pageId"], "target-1");
    assert_eq!(human["timeoutMs"], 1_500);

    let result = serde_json::to_value(Outgoing::CommandResult {
        session_id: "session-1".to_owned(),
        request_id: "request-1".to_owned(),
        ok: false,
        error: Some("nope".to_owned()),
    })
    .expect("command result serializes");
    assert_eq!(result["type"], "command_result");
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"], "nope");
}

#[test]
fn incoming_frames_tolerate_extra_fields_and_unknown_tags() {
    let yield_request: Incoming = serde_json::from_value(json!({
        "type": "yield_request",
        "sessionId": "session-1",
        "pageId": "target-1",
        "reason": "takeover",
        "timeoutMs": 250,
    }))
    .expect("yield request parses");
    assert_eq!(
        yield_request,
        Incoming::YieldRequest {
            page_id: "target-1".to_owned(),
            reason: YieldReason::Takeover,
            timeout_ms: 250,
        }
    );

    let command: Incoming = serde_json::from_value(json!({
        "type": "command",
        "sessionId": "session-1",
        "requestId": "request-1",
        "pageId": "target-1",
        "command": {"kind": "history", "delta": -1},
    }))
    .expect("command parses");
    assert_eq!(
        command,
        Incoming::Command {
            request_id: "request-1".to_owned(),
            page_id: "target-1".to_owned(),
            command: BrowserCommand::History { delta: -1 },
        }
    );

    let unknown_command: Incoming = serde_json::from_value(json!({
        "type": "command",
        "sessionId": "session-1",
        "requestId": "request-1",
        "pageId": "target-1",
        "command": {"kind": "zoom", "level": 2},
    }))
    .expect("an unknown command kind is tolerated");
    assert_eq!(
        unknown_command,
        Incoming::Command {
            request_id: "request-1".to_owned(),
            page_id: "target-1".to_owned(),
            command: BrowserCommand::Unknown,
        }
    );

    let human: Incoming = serde_json::from_value(json!({
        "type": "human_result",
        "sessionId": "session-1",
        "requestId": "request-1",
        "status": "done",
        "url": "https://example.test/after",
        "title": "After",
    }))
    .expect("human result parses");
    assert_eq!(
        human,
        Incoming::HumanResult {
            request_id: "request-1".to_owned(),
            status: HumanStatus::Done,
            url: "https://example.test/after".to_owned(),
            title: "After".to_owned(),
        }
    );

    let unknown: Incoming =
        serde_json::from_value(json!({"type": "viewer_joined"})).expect("unknown tags are ignored");
    assert_eq!(unknown, Incoming::Unknown);
}

#[tokio::test]
async fn without_a_socket_nothing_is_registered() {
    let (inbox, _receiver) = mpsc::unbounded_channel();
    let client = GatewayClient::new(&config(None), "session-1", inbox);
    assert!(!client.is_configured());
    assert!(!client.is_connected());
    assert_eq!(client.sock_path(), None);

    // Every send is a no-op, and start() must not spawn a writer.
    client.start();
    client.update_state(AgentState::Driving);
    client.yield_ok("target-1");
    client.unregister();
    assert!(!client.is_connected());
}

#[tokio::test]
async fn registers_and_replays_state_after_a_reconnect() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sock = dir.path().join("gateway.sock");
    let listener = UnixListener::bind(&sock).expect("bind gateway socket");

    let (inbox, _receiver) = mpsc::unbounded_channel();
    let client = GatewayClient::new(&config(Some(sock.clone())), "session-1", inbox);
    assert!(client.is_configured());
    client.start();

    let mut first = connect(&listener).await;
    let register = next_frame(&mut first).await;
    assert_eq!(register["type"], "register");
    assert_eq!(register["sessionId"], "session-1");
    assert_eq!(register["name"], "test-session");
    assert_eq!(register["cdpUrl"], TEST_CDP_URL);
    assert_eq!(register["state"], "idle");
    assert_eq!(register["pages"].as_array().map(Vec::len), Some(0));

    client.update_state(AgentState::Driving);
    let state = next_frame(&mut first).await;
    assert_eq!(state["type"], "state");
    assert_eq!(state["state"], "driving");

    client.update_pages(vec![page("target-1")]);
    let pages = next_frame(&mut first).await;
    assert_eq!(pages["type"], "update_pages");
    assert_eq!(pages["pages"][0]["targetId"], "target-1");

    // Closing the only handle makes the writer fail, so the loop reconnects. The
    // new register message must carry the state and pages accumulated above.
    drop(first);
    wait_until_disconnected(&client).await;
    client.update_state(AgentState::Idle);

    let mut second = connect(&listener).await;
    let register = next_frame(&mut second).await;
    assert_eq!(register["type"], "register");
    assert_eq!(register["state"], "idle");
    assert_eq!(register["pages"].as_array().map(Vec::len), Some(1));
    assert_eq!(register["pages"][0]["targetId"], "target-1");

    drop(second);
}

#[tokio::test]
async fn malformed_known_frames_are_not_delivered() {
    let dir = tempfile::tempdir().expect("temp dir");
    let sock = dir.path().join("gateway.sock");
    let listener = UnixListener::bind(&sock).expect("bind gateway socket");

    let (inbox, mut receiver) = mpsc::unbounded_channel();
    let client = GatewayClient::new(&config(Some(sock.clone())), "session-1", inbox);
    client.start();

    let mut gateway = connect(&listener).await;
    let register = next_frame(&mut gateway).await;
    assert_eq!(register["type"], "register");

    gateway
        .get_mut()
        .write_all(b"{\"type\":\"reclaimed\",\"pageId\":1}\n")
        .await
        .expect("write malformed frame");
    gateway
        .get_mut()
        .write_all(
            b"{\"type\":\"reclaimed\",\"sessionId\":\"s\",\"pageId\":\"target-1\",\"ok\":true}\n",
        )
        .await
        .expect("write valid frame");

    let message = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("a valid frame arrives")
        .expect("the inbox is open");
    assert_eq!(
        message,
        Incoming::Reclaimed {
            page_id: "target-1".to_owned(),
            ok: true,
        }
    );
}

#[tokio::test]
async fn a_missing_socket_does_not_stop_the_client() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (inbox, _receiver) = mpsc::unbounded_channel();
    let client = GatewayClient::new(
        &config(Some(dir.path().join("missing.sock"))),
        "session-1",
        inbox,
    );
    client.start();
    client.update_state(AgentState::Driving);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!client.is_connected());
}

async fn connect(listener: &UnixListener) -> GatewayStream {
    let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("the client connects")
        .expect("accept");
    BufReader::new(stream)
}

async fn next_frame(gateway: &mut GatewayStream) -> Value {
    let line = tokio::time::timeout(Duration::from_secs(5), read_line(gateway))
        .await
        .expect("a frame arrives")
        .expect("the frame is readable");
    serde_json::from_str(&line).expect("frames are JSON")
}

async fn read_line(gateway: &mut GatewayStream) -> std::io::Result<String> {
    let mut line = String::new();
    let read = gateway.read_line(&mut line).await?;
    assert!(read > 0, "connection closed before a frame arrived");
    Ok(line.trim_end().to_owned())
}

async fn wait_until_disconnected(client: &GatewayClient) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while client.is_connected() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the client never noticed the closed connection"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
