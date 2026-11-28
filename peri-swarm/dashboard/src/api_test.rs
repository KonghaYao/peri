use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;

// ---------------------------------------------------------------------------
// Minimal one-request-per-connection HTTP gateway for wire-level assertions.
// ---------------------------------------------------------------------------

struct MockResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl MockResponse {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: vec![
                ("content-type".to_owned(), "application/json".to_owned()),
                (CONTRACT_HEADER.to_owned(), "1".to_owned()),
            ],
            body: body.to_string(),
        }
    }

    fn empty(status: u16, headers: &[(&str, &str)]) -> Self {
        Self {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: String::new(),
        }
    }
}

struct MockGateway {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl MockGateway {
    fn url(&self) -> &str {
        &self.base_url
    }

    fn finish(mut self) -> Vec<String> {
        if let Some(handle) = self.handle.take() {
            handle.join().expect("mock gateway thread panicked");
        }
        Arc::try_unwrap(self.requests)
            .expect("mock gateway still holds the request log")
            .into_inner()
            .expect("mock gateway request log poisoned")
    }
}

fn spawn_gateway(responses: Vec<MockResponse>) -> MockGateway {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock gateway");
    let address = listener.local_addr().expect("mock gateway address");
    listener
        .set_nonblocking(true)
        .expect("nonblocking mock listener");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        for response in responses {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "mock gateway timed out waiting for a request"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("mock gateway accept failed: {error}"),
                }
            };
            stream
                .set_nonblocking(false)
                .expect("mock connection is blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("mock read timeout");
            let request = read_request(&mut stream);
            captured
                .lock()
                .expect("mock request log poisoned")
                .push(request);
            stream
                .write_all(&render_response(&response))
                .expect("write mock response");
            stream.flush().expect("flush mock response");
        }
    });
    MockGateway {
        base_url: format!("http://{address}"),
        requests,
        handle: Some(handle),
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).expect("read mock request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        let Some(headers_end) = find_headers_end(&bytes) else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..headers_end]).to_string();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        let body_start = headers_end + 4;
        while bytes.len() < body_start + content_length {
            let read = stream.read(&mut buffer).expect("read mock request body");
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        break;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn find_headers_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn render_response(response: &MockResponse) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    head.push_str(&response.body);
    head.into_bytes()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        502 => "Bad Gateway",
        _ => "Status",
    }
}

fn request_line(request: &str) -> &str {
    request.lines().next().unwrap_or_default()
}

fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request
        .split("\r\n\r\n")
        .next()
        .unwrap_or_default()
        .lines()
        .skip(1)
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
}

fn request_json(request: &str) -> Value {
    let body = request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    serde_json::from_str(body).expect("request body is JSON")
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const KEY: &str = "synthetic-test-key-never-real";
const AUTH_HEADER: &str = "Bearer synthetic-test-key-never-real";

fn session_json(id: &str) -> Value {
    json!({
        "id": id,
        "object": "agent.session",
        "logicalId": "logical-a",
        "rootSessionId": "ses_root",
        "cwd": "/tmp/work",
        "state": "idle",
        "status": "requires_action",
        "turn": { "requestId": "req_1", "startedAt": "2026-01-01T00:00:00Z" },
        "lastItemSeq": 7,
        "created_at": 1767225600,
        "last_active_at": 1767225660,
        "agent": { "model": "model-a", "instructions": "be brief" },
        "environment": { "type": "none" },
        "sandbox": null,
        "required_actions": [{
            "type": "mcp_approval_request",
            "request_id": "appr_1",
            "turn_id": "turn_1",
            "request": {
                "type": "mcp_tool_call",
                "server_label": "server-a",
                "name": "search",
                "arguments": { "q": "x" },
                "message": "approval needed"
            }
        }],
        "vault_ids": [],
        "usage": null,
        "error": null,
        "metadata": { "team": "platform" },
        "prompt_cache_options": { "ttl": "30m" }
    })
}

fn client(url: &str) -> HttpSwarmApi {
    HttpSwarmApi::new(url, KEY).expect("build client")
}

// ---------------------------------------------------------------------------
// Wire behavior
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agent_metrics_uses_the_bounded_window_and_keeps_missing_tokens_none() {
    let gateway = spawn_gateway(vec![MockResponse::json(
        200,
        json!({
            "start_time": 100,
            "end_time": 200,
            "sessions": [{
                "session_id": "ses_1",
                "agent_id": "root",
                "model": "model-a",
                "input_tokens": 10,
                "total_tokens": 30,
                "turns": 2,
                "calls": 3,
                "last_active_at": "2026-01-01T00:00:00Z"
            }],
            "totals": {
                "input_tokens": 10,
                "turns": 2,
                "calls": 3,
                "model": "model-a"
            },
            "truncated": true
        }),
    )]);
    let api = client(gateway.url());

    let metrics = api
        .agent_metrics(100, 200, 500)
        .await
        .expect("agent metrics");
    assert_eq!(metrics.start_time, 100);
    assert_eq!(metrics.end_time, 200);
    assert_eq!(metrics.sessions.len(), 1);
    assert_eq!(metrics.sessions[0].session_id, "ses_1");
    assert_eq!(metrics.sessions[0].input_tokens, Some(10));
    assert_eq!(metrics.sessions[0].output_tokens, None);
    assert_eq!(metrics.sessions[0].cache_read_tokens, None);
    assert_eq!(metrics.sessions[0].cache_creation_tokens, None);
    assert_eq!(metrics.totals.output_tokens, None);
    assert!(metrics.truncated);

    let requests = gateway.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "GET /v1/agents/metrics?start_time=100&end_time=200&limit=500 HTTP/1.1"
    );
    assert_eq!(
        header_value(&requests[0], "authorization"),
        Some(AUTH_HEADER)
    );
    assert_eq!(
        header_value(&requests[0], EXPECT_HEADER),
        Some(CONTRACT_VERSION)
    );
}

#[tokio::test]
async fn agent_metrics_rejects_a_limit_above_the_contract_max() {
    let api = HttpSwarmApi::new("http://127.0.0.1:9", KEY).expect("client");
    let error = api
        .agent_metrics(100, 200, MAX_PAGE_LIMIT + 1)
        .await
        .expect_err("limit is rejected locally");
    assert!(error.message().contains("limit"));
}

#[tokio::test]
async fn list_sessions_sends_contract_headers_and_decodes_the_page() {
    let gateway = spawn_gateway(vec![MockResponse::json(
        200,
        json!({
            "object": "list",
            "data": [session_json("ses_1")],
            "has_more": true,
            "hasMore": true,
            "nextCursor": "ses_next"
        }),
    )]);
    let api = client(gateway.url());

    let page = api.list_sessions(50).await.expect("list sessions");
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].id, "ses_1");
    assert_eq!(
        page.data[0].metadata.get("team").map(String::as_str),
        Some("platform")
    );
    assert_eq!(page.data[0].agent.model.as_deref(), Some("model-a"));
    assert_eq!(page.data[0].agent.instructions.as_deref(), Some("be brief"));
    assert_eq!(page.data[0].environment.kind, "none");
    assert!(page.data[0].environment.id.is_none());
    assert_eq!(page.data[0].required_actions.len(), 1);
    assert_eq!(
        page.data[0]
            .prompt_cache_options
            .as_ref()
            .map(|options| options.ttl.as_str()),
        Some("30m")
    );
    assert_eq!(page.data[0].created_at, 1_767_225_600);
    assert_eq!(page.data[0].last_active_at, 1_767_225_660);
    assert_eq!(page.data[0].turn.request_id.as_deref(), Some("req_1"));
    assert!(page.has_more);
    assert_eq!(page.next_cursor.as_deref(), Some("ses_next"));
    assert_eq!(api.contract_version().as_deref(), Some("1"));

    let requests = gateway.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        request_line(&requests[0]),
        "GET /v1/agents/sessions?limit=50&order=desc HTTP/1.1"
    );
    assert_eq!(
        header_value(&requests[0], "authorization"),
        Some("Bearer synthetic-test-key-never-real")
    );
    assert_eq!(header_value(&requests[0], "x-peri-swarm-expect"), Some("1"));
}

#[tokio::test]
async fn health_omits_the_bearer_and_keeps_the_last_contract_version() {
    let gateway = spawn_gateway(vec![
        MockResponse {
            status: 200,
            headers: vec![
                ("content-type".to_owned(), "application/json".to_owned()),
                (CONTRACT_HEADER.to_owned(), "2".to_owned()),
            ],
            body: json!({ "data": [], "has_more": false }).to_string(),
        },
        MockResponse {
            status: 200,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: json!({ "status": "ok" }).to_string(),
        },
    ]);
    let api = client(gateway.url());

    api.list_sessions(1).await.expect("list sessions");
    assert_eq!(api.contract_version().as_deref(), Some("2"));

    let health = api.health().await.expect("health");
    assert_eq!(health.status, "ok");
    // Health does not carry the contract header and must not clear the value.
    assert_eq!(api.contract_version().as_deref(), Some("2"));

    let requests = gateway.finish();
    assert_eq!(request_line(&requests[1]), "GET /healthz HTTP/1.1");
    assert!(header_value(&requests[1], "authorization").is_none());
    assert_eq!(header_value(&requests[1], "x-peri-swarm-expect"), Some("1"));
}

#[tokio::test]
async fn write_retries_reuse_the_idempotency_key_and_body() {
    let gateway = spawn_gateway(vec![
        MockResponse::json(
            201,
            json!({ "inputId": "in_1", "accepted": true, "disposition": "inserted", "mode": "turn", "lastItemSeq": 8 }),
        ),
        MockResponse::json(
            201,
            json!({ "inputId": "in_1", "accepted": true, "disposition": "inserted", "mode": "turn", "lastItemSeq": 8 }),
        ),
    ]);
    let api = client(gateway.url());
    let key = IdempotencyKey::new();

    let first = api
        .send_input("ses_1", "hello", &key)
        .await
        .expect("first send");
    // Simulate a retry of the same user submission.
    let second = api
        .send_input("ses_1", "hello", &key)
        .await
        .expect("retry send");
    assert_eq!(first, second);
    assert_eq!(first.input_id, "in_1");
    assert_eq!(first.last_item_seq, 8);

    let requests = gateway.finish();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        request_line(&requests[0]),
        "POST /v1/agents/sessions/ses_1/events HTTP/1.1"
    );
    for request in &requests {
        assert_eq!(header_value(request, "idempotency-key"), Some(key.as_str()));
        assert_eq!(request_json(request), message_event_body("hello"));
        assert!(header_value(request, "content-type")
            .is_some_and(|value| value.starts_with("application/json")));
    }
}

#[tokio::test]
async fn page_and_mutation_endpoints_use_the_documented_routes() {
    let gateway = spawn_gateway(vec![
        // turns
        MockResponse::json(
            200,
            json!({ "object": "list", "data": [], "has_more": false, "nextCursor": null }),
        ),
        // items
        MockResponse::json(
            200,
            json!({
                "object": "list",
                "items": [],
                "data": [],
                "next_cursor": 41,
                "lastItemSeq": 41,
                "has_more": false,
                "order": "desc"
            }),
        ),
        // update
        MockResponse::json(200, session_json("ses_1")),
        // delete
        MockResponse::empty(204, &[]),
        // cancel: the gateway accepts events with 202.
        MockResponse::json(
            202,
            json!({ "inputId": "in_cancel", "accepted": true, "disposition": "inserted", "mode": "turn", "lastItemSeq": 9 }),
        ),
        // approval
        MockResponse::json(
            202,
            json!({ "requestId": "appr_1", "decision": "deny", "reason": "user_denied", "inputId": "in_answer", "disposition": "applied" }),
        ),
        // send input
        MockResponse::json(
            202,
            json!({ "inputId": "in_send", "accepted": true, "disposition": "inserted", "mode": "turn", "lastItemSeq": 10 }),
        ),
    ]);
    let api = client(gateway.url());
    let key = IdempotencyKey::new();

    let turns = api
        .list_turns("ses_1", 50, Some("turn_9"))
        .await
        .expect("turns");
    assert!(turns.data.is_empty());

    let items = api
        .list_items("ses_1", 50, Some("41"))
        .await
        .expect("items");
    assert_eq!(items.next_cursor_text().as_deref(), Some("41"));

    let updated = api
        .update_session(
            "ses_1",
            &UpdateSessionSpec {
                model: Some("model-b".to_owned()),
                ..UpdateSessionSpec::default()
            },
            &key,
        )
        .await
        .expect("update");
    assert_eq!(updated.id, "ses_1");

    api.delete_session("ses_1", &key).await.expect("delete");

    let cancelled = api.cancel("ses_1", &key).await.expect("cancel");
    assert_eq!(cancelled.disposition, "inserted");

    let approval = api
        .approve("ses_1", "appr_1", ApprovalDecision::Deny, &key)
        .await
        .expect("approval");
    assert_eq!(approval.decision.as_deref(), Some("deny"));

    let sent = api
        .send_input("ses_1", "later", &key)
        .await
        .expect("send input");
    assert_eq!(sent.last_item_seq, 10);

    let requests = gateway.finish();
    assert_eq!(requests.len(), 7);
    assert_eq!(
        request_line(&requests[0]),
        "GET /v1/agents/sessions/ses_1/turns?limit=50&order=desc&after=turn_9 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[1]),
        "GET /v1/agents/sessions/ses_1/items?limit=50&order=desc&after=41 HTTP/1.1"
    );
    assert_eq!(
        request_line(&requests[2]),
        "POST /v1/agents/sessions/ses_1 HTTP/1.1"
    );
    assert_eq!(
        request_json(&requests[2]),
        json!({ "agent": { "model": "model-b" } })
    );
    assert_eq!(
        request_line(&requests[3]),
        "DELETE /v1/agents/sessions/ses_1 HTTP/1.1"
    );
    assert_eq!(
        request_json(&requests[4]),
        cancel_event_body(),
        "cancel body"
    );
    assert_eq!(
        request_json(&requests[5]),
        approval_event_body("appr_1", ApprovalDecision::Deny),
        "approval body"
    );
    assert_eq!(
        request_json(&requests[6]),
        message_event_body("later"),
        "message body"
    );
    for request in &requests[2..] {
        assert_eq!(header_value(request, "idempotency-key"), Some(key.as_str()));
    }
    for request in &requests {
        assert_eq!(header_value(request, "authorization"), Some(AUTH_HEADER));
        assert_eq!(header_value(request, "x-peri-swarm-expect"), Some("1"));
    }
}

#[tokio::test]
async fn create_session_decodes_the_created_flag() {
    let mut body = session_json("ses_new");
    body["created"] = Value::Bool(true);
    let gateway = spawn_gateway(vec![MockResponse::json(201, body)]);
    let api = client(gateway.url());

    let created = api
        .create_session(&CreateSessionSpec::default(), &IdempotencyKey::new())
        .await
        .expect("create session");
    assert!(created.created);
    assert_eq!(created.session.id, "ses_new");

    let requests = gateway.finish();
    assert_eq!(
        request_line(&requests[0]),
        "POST /v1/agents/sessions HTTP/1.1"
    );
    assert!(header_value(&requests[0], "idempotency-key").is_some());
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let gateway = spawn_gateway(vec![MockResponse::empty(
        302,
        &[("location", "http://127.0.0.1:1/elsewhere")],
    )]);
    let api = client(gateway.url());

    let error = api.get_session("ses_1").await.expect_err("redirect");
    assert_eq!(error.kind(), ApiErrorKind::Redirect);
    assert_eq!(error.status(), Some(302));
    // Exactly one request reached the configured origin.
    assert_eq!(gateway.finish().len(), 1);
}

#[tokio::test]
async fn page_limit_is_validated_before_sending() {
    let api = client("http://127.0.0.1:9");
    for limit in [0, MAX_PAGE_LIMIT + 1] {
        let error = api.list_sessions(limit).await.expect_err("invalid limit");
        assert_eq!(error.kind(), ApiErrorKind::BadRequest);
        assert!(error.status().is_none());
    }
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

#[test]
fn create_body_omits_blank_fields_and_preserves_advanced_json() {
    let spec = CreateSessionSpec {
        logical_id: Some("   ".to_owned()),
        cwd: Some("/tmp/work".to_owned()),
        model: Some(String::new()),
        instructions: Some("be brief".to_owned()),
        metadata: BTreeMap::from([("team".to_owned(), "platform".to_owned())]),
        tools: Some(json!([{ "type": "mcp" }])),
        environment: None,
        prompt_cache_options: None,
    };
    assert_eq!(
        create_session_body(&spec),
        json!({
            "cwd": "/tmp/work",
            "instructions": "be brief",
            "metadata": { "team": "platform" },
            "tools": [{ "type": "mcp" }],
            "stream": false
        })
    );
    assert_eq!(
        create_session_body(&CreateSessionSpec::default()),
        json!({ "stream": false })
    );
}

#[test]
fn update_body_uses_the_gateway_agent_wrapper() {
    let spec = UpdateSessionSpec {
        model: Some("model-b".to_owned()),
        metadata: Some(BTreeMap::new()),
        prompt_cache_options: Some(Value::Null),
    };
    assert_eq!(
        update_session_body(&spec).expect("update body"),
        json!({
            "agent": { "model": "model-b" },
            "metadata": {},
            "prompt_cache_options": null
        })
    );
    let error = update_session_body(&UpdateSessionSpec {
        model: Some("  ".to_owned()),
        ..UpdateSessionSpec::default()
    })
    .expect_err("blank model");
    assert_eq!(error.kind(), ApiErrorKind::BadRequest);
    assert_eq!(error.message(), "model must not be empty");
}

#[test]
fn event_bodies_match_the_official_envelope() {
    assert_eq!(
        message_event_body("hello"),
        json!({
            "events": [{
                "type": "agent.session.input.message",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "hello" }]
                }]
            }]
        })
    );
    assert_eq!(
        cancel_event_body(),
        json!({ "events": [{ "type": "agent.session.input.cancel" }] })
    );
    assert_eq!(
        approval_event_body("appr_1", ApprovalDecision::Approve),
        json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "request_id": "appr_1",
                "response": { "type": "mcp_tool_call", "decision": "approve" }
            }]
        })
    );
    assert_eq!(
        approval_event_body("appr_1", ApprovalDecision::Deny)["events"][0]["response"]["decision"],
        json!("deny")
    );
}

#[test]
fn idempotency_keys_are_unique_and_reusable() {
    let first = IdempotencyKey::new();
    let second = IdempotencyKey::new();
    assert_ne!(first, second);
    assert_eq!(first.as_str(), first.clone().as_str());
    assert!(uuid::Uuid::parse_str(first.as_str()).is_ok());
}

// ---------------------------------------------------------------------------
// DTO decoding
// ---------------------------------------------------------------------------

#[test]
fn page_dtos_accept_both_has_more_spellings() {
    let snake: SessionPage = serde_json::from_value(json!({
        "data": [],
        "has_more": true,
        "next_cursor": "ses_next"
    }))
    .expect("snake page");
    assert!(snake.has_more);
    assert_eq!(snake.next_cursor.as_deref(), Some("ses_next"));

    let camel: SessionPage = serde_json::from_value(json!({
        "data": [],
        "hasMore": false,
        "nextCursor": null
    }))
    .expect("camel page");
    assert!(!camel.has_more);
    assert!(camel.next_cursor.is_none());

    // The real gateway emits both keys on the same object; this must not be
    // treated as a duplicate field.
    let both: SessionPage = serde_json::from_value(json!({
        "data": [],
        "has_more": true,
        "hasMore": true,
        "next_cursor": "ses_snake",
        "nextCursor": "ses_camel"
    }))
    .expect("both page spellings");
    assert!(both.has_more);
    assert_eq!(both.next_cursor.as_deref(), Some("ses_camel"));

    let turns: Turns = serde_json::from_value(json!({
        "data": [],
        "has_more": false,
        "hasMore": false,
        "next_cursor": "turn_snake",
        "nextCursor": "turn_camel"
    }))
    .expect("turn page");
    assert!(!turns.has_more);
    assert_eq!(turns.next_cursor.as_deref(), Some("turn_camel"));
}

#[test]
fn turns_page_decodes_the_canonical_turn() {
    let page: Turns = serde_json::from_value(json!({
        "object": "list",
        "data": [{
            "id": "turn_1",
            "object": "agent.session.turn",
            "agent_id": "root",
            "session_id": "ses_1",
            "status": "completed",
            "created_at": 1767225600,
            "started_at": 1767225601,
            "completed_at": 1767225610,
            "subagent_id": null,
            "error": null,
            "usage": { "total_tokens": 10 }
        }],
        "has_more": false,
        "nextCursor": "turn_1"
    }))
    .expect("turns page");
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].status, "completed");
    assert_eq!(page.data[0].started_at, Some(1_767_225_601));
    assert_eq!(page.data[0].completed_at, Some(1_767_225_610));
    assert!(!page.has_more);
}

#[test]
fn items_page_keeps_raw_and_official_views() {
    let page: ItemsPage = serde_json::from_value(json!({
        "items": [{ "id": "item_1" }],
        "data": [{ "type": "message", "role": "user" }],
        "next_cursor": 41,
        "nextCursor": 42,
        "has_more": true,
        "hasMore": true,
        "order": "desc",
        "last_item_seq": 41,
        "lastItemSeq": 42
    }))
    .expect("items page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.data.len(), 1);
    assert!(page.has_more);
    assert_eq!(page.order.as_deref(), Some("desc"));
    assert_eq!(page.last_item_seq, Some(42));
    assert_eq!(page.next_cursor_text().as_deref(), Some("42"));
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

#[test]
fn error_statuses_map_to_stable_kinds() {
    let cases = [
        (400, ApiErrorKind::BadRequest),
        (401, ApiErrorKind::Unauthorized),
        (404, ApiErrorKind::NotFound),
        (409, ApiErrorKind::Conflict),
        (422, ApiErrorKind::Validation),
        (502, ApiErrorKind::BadGateway),
    ];
    for (status, kind) in cases {
        let code = format!("code_{status}");
        let body = json!({
            "error": {
                "code": code,
                "message": "safe gateway message",
                "details": { "field": "cwd" }
            }
        })
        .to_string();
        let error = ApiError::from_status(status, body.as_bytes());
        assert_eq!(error.kind(), kind);
        assert_eq!(error.status(), Some(status));
        assert_eq!(error.code(), Some(code.as_str()));
        assert_eq!(error.message(), "safe gateway message");
        assert_eq!(error.details(), &json!({ "field": "cwd" }));
    }
}

#[test]
fn error_predicates_cover_actionable_classes() {
    let unauthorized = ApiError::from_status(401, b"{}");
    assert!(unauthorized.is_unauthorized());
    assert!(!unauthorized.is_retryable());

    let conflict = ApiError::from_status(409, b"{}");
    assert!(conflict.is_conflict());

    let bad_gateway = ApiError::from_status(502, b"{}");
    assert!(bad_gateway.is_bad_gateway());
    assert!(bad_gateway.is_retryable());

    let missing = ApiError::from_status(404, b"{}");
    assert!(missing.is_not_found());
}

#[test]
fn non_json_error_bodies_are_not_echoed() {
    let error = ApiError::from_status(500, b"<html>internal detail</html>");
    assert_eq!(error.kind(), ApiErrorKind::Server);
    assert_eq!(error.code(), None);
    assert_eq!(error.message(), "the gateway failed to process the request");
    assert!(!error.message().contains("html"));
}

#[test]
fn transport_failures_keep_a_structured_cause_and_raw_detail() {
    let cases = [
        (ApiErrorCause::ClientBuild, ApiErrorKind::Network),
        (ApiErrorCause::Request, ApiErrorKind::Network),
        (ApiErrorCause::ResponseRead, ApiErrorKind::Network),
        (ApiErrorCause::ResponseDecode, ApiErrorKind::Decode),
    ];
    for (cause, kind) in cases {
        let error = ApiError::failed(cause, "connection refused");
        assert_eq!(error.kind(), kind);
        assert_eq!(error.cause(), Some(cause));
        assert_eq!(error.message(), "connection refused");
        assert_eq!(error.status(), None);
        assert_eq!(error.code(), None);
    }
}

#[tokio::test]
async fn undecodable_success_body_is_a_protocol_error() {
    let gateway = spawn_gateway(vec![MockResponse {
        status: 200,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: "not json".to_owned(),
    }]);
    let api = client(gateway.url());

    let error = api.get_session("ses_1").await.expect_err("decode failure");
    assert_eq!(error.kind(), ApiErrorKind::Decode);
    assert_eq!(error.cause(), Some(ApiErrorCause::ResponseDecode));
    assert!(error.status().is_none());
    assert!(
        crate::ui::text::api_error(&error).starts_with("Gateway response could not be decoded: ")
    );
    assert_eq!(gateway.finish().len(), 1);
}
