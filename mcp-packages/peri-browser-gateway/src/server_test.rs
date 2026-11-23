use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, ContentBlock,
    GetSkillRequestParams, Implementation, ProtocolVersion, ReadResourceRequestParams,
    ResourceContents, SkillResources,
};
use rmcp::service::{
    serve_client_with_lifecycle, ClientLifecycleMode, RoleClient, RunningService, ServiceError,
};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::config::Config;
use crate::skills::skill_uri;

const AUTHORIZE_URL: &str = "https://example.test/oauth/authorize?client=peri";
const AUTHORIZE_REASON: &str = "connect the deployment account";

/// 客户端不声明 elicitation；授权工具只返回结果，不得发起 peer 请求。
async fn with_client<F, Fut>(body: F) -> anyhow::Result<()>
where
    F: FnOnce(RunningService<RoleClient, ClientConfig>) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let home = tempfile::tempdir()?;
    let offline = TcpListener::bind("127.0.0.1:0").await?;
    let suggested = format!("http://{}", offline.local_addr()?);
    // 端口保持由 fixture 持有，但不 accept；不会探测用户或默认端口上的引擎。
    let config = Config::parse_from(["--cdp-url".to_owned(), suggested], |key| match key {
        "HOME" => Some(home.path().to_string_lossy().into_owned()),
        "PERI_BROWSER_PROVISION_LOCK" => Some(
            home.path()
                .join("provision.lock")
                .to_string_lossy()
                .into_owned(),
        ),
        "PERI_BROWSER_CONNECT_TIMEOUT_MS" => Some("150".to_owned()),
        _ => None,
    })?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let uri = format!("http://{}/mcp", listener.local_addr()?);
    let stop = CancellationToken::new();
    let _on_exit = stop.clone().drop_guard();
    let server = tokio::spawn(crate::serve(
        listener,
        config,
        stop.clone().cancelled_owned(),
    ));
    let transport = StreamableHttpClientTransport::with_client(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()?,
        StreamableHttpClientTransportConfig::with_uri(uri),
    );
    let info = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("peri-browser-test-client", "0.0.0"),
    )
    .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE);
    let client =
        serve_client_with_lifecycle(info, transport, ClientLifecycleMode::Initialize).await?;
    let result = body(client).await;
    stop.cancel();
    server.await??;
    result
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

async fn call(
    client: &RunningService<RoleClient, ClientConfig>,
    name: &str,
    arguments: Value,
) -> Result<CallToolResult, ServiceError> {
    client
        .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(object(arguments)))
        .await
}

fn error_code(result: &CallToolResult) -> String {
    assert_eq!(result.is_error, Some(true), "expected an error result");
    result
        .structured_content
        .as_ref()
        .expect("error results are structured")["code"]
        .as_str()
        .expect("every error has a code")
        .to_owned()
}

#[test]
fn tool_surface_matches_the_readme() {
    let tools = BrowserMcpServer::tools();
    let names: Vec<&str> = tools.iter().map(|tool| &*tool.name).collect();
    assert_eq!(
        names,
        vec![
            "browser_status",
            "browser_connect",
            "list_pages",
            "select_page",
            "new_page",
            "close_page",
            "navigate_page",
            "take_snapshot",
            "take_screenshot",
            "evaluate_script",
            "click",
            "fill",
            "hover",
            "press_key",
            "wait_for",
            "list_console_messages",
            "list_network_requests",
            "get_network_request",
            "yield_page",
            "request_human",
            "request_authorization",
        ],
        "README §7 tool table"
    );
}

#[test]
fn every_tool_has_a_description_and_an_object_schema() {
    for tool in BrowserMcpServer::tools() {
        assert!(
            tool.description
                .as_deref()
                .is_some_and(|description| !description.is_empty()),
            "{} has no description",
            tool.name
        );
        assert_eq!(
            tool.input_schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{} must take an object",
            tool.name
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn request_authorization_returns_the_url_without_a_browser() -> anyhow::Result<()> {
    with_client(|client| async move {
        let result = call(
            &client,
            "request_authorization",
            json!({"url": AUTHORIZE_URL, "reason": AUTHORIZE_REASON}),
        )
        .await?;
        assert_eq!(result.is_error, Some(false), "{result:?}");
        let structured = result.structured_content.expect("structured result");
        assert_eq!(structured["status"], "user_confirmation_required");
        assert_eq!(structured["url"], AUTHORIZE_URL);
        assert_eq!(structured["message"], AUTHORIZE_REASON);
        let instruction = structured["instruction"].as_str().expect("instruction");
        assert!(
            instruction.contains("AskUserQuestion"),
            "the model is told which question tool to use: {instruction}"
        );
        let text = result
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text(text) => Some(&text.text),
                _ => None,
            })
            .expect("the result carries a text explanation");
        assert!(text.contains(AUTHORIZE_URL), "{text}");

        // Without a reason the message still explains the hand-off.
        let defaulted = call(
            &client,
            "request_authorization",
            json!({"url": AUTHORIZE_URL}),
        )
        .await?;
        let structured = defaulted.structured_content.expect("structured result");
        assert_eq!(structured["status"], "user_confirmation_required");
        assert!(
            structured["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "the message defaults instead of going empty"
        );

        // No browser endpoint is configured in this fixture: authorization
        // never needs one.
        let status = call(&client, "browser_status", json!({})).await?;
        assert_eq!(
            status.structured_content.expect("structured status")["connected"],
            false
        );
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn request_authorization_validates_the_url() -> anyhow::Result<()> {
    with_client(|client| async move {
        for url in [
            "not a url",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://",
        ] {
            let result = call(&client, "request_authorization", json!({"url": url})).await?;
            assert_eq!(error_code(&result), "invalid_argument", "{url} is rejected");
        }
        let status = call(&client, "browser_status", json!({})).await?;
        assert_eq!(status.is_error, Some(false));
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn skills_and_resources_match_the_bundled_files() -> anyhow::Result<()> {
    with_client(|client| async move {
        let expected = [
            (
                "browser-automation",
                include_str!("../skills/browser-automation/SKILL.md"),
            ),
            ("debugging", include_str!("../skills/debugging/SKILL.md")),
        ];

        let skills = client.list_all_skills().await?;
        assert_eq!(skills.len(), expected.len());
        for (skill, (name, text)) in skills.iter().zip(expected) {
            let uri = skill_uri(name);
            assert_eq!(skill.uri, uri);
            assert_eq!(skill.frontmatter.name, name);
            assert!(!skill.frontmatter.description.is_empty());

            let SkillResources::Files(files) = &skill.resources else {
                panic!("bundled skills always list fixed files");
            };
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].uri, uri);
            assert_eq!(files[0].size, text.len() as u64);
            assert_eq!(files[0].digest, crate::skills::digest(text.as_bytes()));

            let read = client
                .read_resource(ReadResourceRequestParams::new(uri.clone()))
                .await?;
            assert_eq!(read.contents.len(), 1);
            match &read.contents[0] {
                ResourceContents::TextResourceContents {
                    uri: read_uri,
                    mime_type,
                    text: body,
                    ..
                } => {
                    assert_eq!(read_uri, &uri);
                    assert_eq!(mime_type.as_deref(), Some("text/markdown"));
                    assert_eq!(body.as_bytes(), text.as_bytes());
                }
                other => panic!("expected text contents, got {other:?}"),
            }

            let skill = client.get_skill(GetSkillRequestParams::new(uri)).await?;
            assert_eq!(skill.skill.frontmatter.name, name);
        }

        let resources = client.list_all_resources().await?;
        assert_eq!(resources.len(), expected.len());
        for resource in resources {
            assert_eq!(resource.mime_type.as_deref(), Some("text/markdown"));
            assert!(resource.size.is_some_and(|size| size > 0));
            assert!(resource.title.is_some_and(|title| !title.is_empty()));
        }
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn tool_failures_are_results_with_stable_codes() -> anyhow::Result<()> {
    with_client(|client| async move {
        let status = call(&client, "browser_status", json!({})).await?;
        assert_eq!(status.is_error, Some(false));
        let structured = status.structured_content.expect("structured status");
        assert_eq!(structured["connected"], false);
        assert_eq!(structured["monitor"], "waiting_for_connect");

        for (name, arguments, code) in [
            ("take_snapshot", json!({}), "not_connected"),
            ("take_screenshot", json!({}), "not_connected"),
            (
                "navigate_page",
                json!({"url": "example.test"}),
                "not_connected",
            ),
            ("list_console_messages", json!({}), "not_connected"),
            ("list_network_requests", json!({}), "not_connected"),
            (
                "get_network_request",
                json!({"requestId": "r-1"}),
                "not_connected",
            ),
            (
                "select_page",
                json!({"pageId": "target-1"}),
                "page_not_found",
            ),
            ("close_page", json!({}), "no_active_page"),
            ("yield_page", json!({}), "not_connected"),
            (
                "request_human",
                json!({"prompt": "log in"}),
                "not_connected",
            ),
            (
                "request_human",
                json!({"prompt": "   "}),
                "invalid_argument",
            ),
            (
                "evaluate_script",
                json!({"expression": "  "}),
                "invalid_argument",
            ),
        ] {
            let result = call(&client, name, arguments).await?;
            assert_eq!(error_code(&result), code, "{name}");
            let structured = result.structured_content.expect("structured error");
            assert!(
                structured["error"].is_string(),
                "{name} explains the failure"
            );
            assert!(
                structured.get("hint").is_some(),
                "{name} carries a hint field"
            );
        }
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn browser_status_reports_the_onboarding_state() -> anyhow::Result<()> {
    with_client(|client| async move {
        let status = call(&client, "browser_status", json!({})).await?;
        assert_eq!(status.is_error, Some(false), "{status:?}");
        let status = status.structured_content.expect("structured status");
        assert_eq!(status["connected"], false);
        assert_eq!(status["platform"]["os"], std::env::consts::OS);
        assert_eq!(status["platform"]["arch"], std::env::consts::ARCH);
        assert!(status["platform"]["supported"].is_boolean());
        assert!(status["kitesurf"]["installCommand"]
            .as_str()
            .is_some_and(|command| command.contains("install-kitesurf.sh")));
        let suggested = status["recommendedEndpoint"]["url"].as_str().unwrap();
        assert!(suggested.starts_with("http://127.0.0.1:"));
        assert_eq!(status["recommendedEndpoint"]["listening"], false);
        assert!(
            status["nextSteps"]
                .as_array()
                .is_some_and(|steps| !steps.is_empty()),
            "a disconnected session always explains the next step: {status:?}"
        );
        assert_eq!(status["monitor"], "waiting_for_connect");
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn browser_connect_validates_then_reports_an_unreachable_endpoint() -> anyhow::Result<()> {
    with_client(|client| async move {
        for url in [
            "https://127.0.0.1:18971",
            "wss://localhost:18971",
            "http://127.0.0.1",
            "http://example.test:18971",
            "file:///tmp/kitesurf.sock",
        ] {
            let result = call(&client, "browser_connect", json!({"url": url})).await?;
            assert_eq!(error_code(&result), "invalid_argument", "{url}");
        }

        let result = call(
            &client,
            "browser_connect",
            json!({"url": "http://127.0.0.1:1"}),
        )
        .await?;
        assert_eq!(error_code(&result), "connect_failed");
        assert!(
            result.structured_content.expect("error content")["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("browser_connect")),
            "the failure explains how to retry"
        );

        let status = call(&client, "browser_status", json!({})).await?;
        let status = status.structured_content.expect("structured status");
        assert_eq!(status["connected"], false);
        assert!(
            status["endpoint"].is_null(),
            "a failed connect changes nothing"
        );
        Ok(())
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn browser_connect_attaches_once_and_refuses_a_second_endpoint() -> anyhow::Result<()> {
    let (address, server, _seen) = fake_cdp().await;
    let other = fake_cdp().await;
    let other_address = other.0;
    with_client(move |client| async move {
        let url = format!("http://{address}");
        let connected = call(&client, "browser_connect", json!({"url": url})).await?;
        assert_eq!(connected.is_error, Some(false), "{connected:?}");
        let value = connected.structured_content.expect("connect result");
        assert_eq!(value["connected"], true);
        assert_eq!(
            value["endpoint"],
            format!("ws://{address}/devtools/browser")
        );
        assert_eq!(value["engine"]["version"]["Browser"], "Kitesurf/0.5.4");

        let again = call(&client, "browser_connect", json!({"url": url})).await?;
        assert_eq!(again.is_error, Some(false), "{again:?}");
        assert_eq!(
            again.structured_content.expect("idempotent result")["alreadyConnected"],
            true
        );

        let refused = call(
            &client,
            "browser_connect",
            json!({"url": format!("http://{other_address}")}),
        )
        .await?;
        assert_eq!(error_code(&refused), "already_connected");

        let status = call(&client, "browser_status", json!({})).await?;
        let status = status.structured_content.expect("structured status");
        assert_eq!(status["connected"], true);
        assert_eq!(
            status["endpoint"],
            format!("ws://{address}/devtools/browser")
        );
        assert_eq!(status["nextSteps"].as_array().map(Vec::len), Some(0));
        Ok(())
    })
    .await?;
    other.1.abort();
    server.abort();
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_tools_and_arguments_are_protocol_errors() -> anyhow::Result<()> {
    with_client(|client| async move {
        let error = call(&client, "no_such_tool", json!({}))
            .await
            .expect_err("unknown tools are protocol errors");
        let ServiceError::McpError(error) = error else {
            anyhow::bail!("预期 JSON-RPC 错误，实际为 {error}");
        };
        assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(error.message, "unknown tool");

        let error = call(&client, "select_page", json!({"pageId": 5}))
            .await
            .expect_err("wrong argument types are protocol errors");
        assert!(
            error
                .to_string()
                .contains("invalid arguments for select_page"),
            "{error}"
        );

        let error = client
            .read_resource(ReadResourceRequestParams::new(
                "skill://peri-browser/no-such-skill/SKILL.md",
            ))
            .await
            .expect_err("unknown resources are protocol errors");
        assert!(error.to_string().contains("no such resource"), "{error}");

        let error = client
            .get_skill(GetSkillRequestParams::new(
                "skill://peri-browser/no-such-skill/SKILL.md",
            ))
            .await
            .expect_err("unknown skills are protocol errors");
        assert!(error.to_string().contains("unknown skill uri"), "{error}");
        Ok(())
    })
    .await
}

/// A page target a fake CDP endpoint hands out: a root without a backend node
/// id and one button that interaction tools can address.
const FAKE_AX_TREE: &str = r#"{
    "nodes": [
        {
            "nodeId": "1",
            "role": {"value": "RootWebArea"},
            "name": {"value": "Example"},
            "childIds": ["2"],
            "ignored": false
        },
        {
            "nodeId": "2",
            "parentId": "1",
            "role": {"value": "button"},
            "name": {"value": "Sign in"},
            "backendDOMNodeId": 42,
            "ignored": false
        }
    ]
}"#;

#[tokio::test(flavor = "current_thread")]
async fn browser_tools_drive_a_cdp_endpoint() -> anyhow::Result<()> {
    let (address, server, seen) = fake_cdp().await;
    with_client(|client| async move {
        let connected = call(
            &client,
            "browser_connect",
            json!({"url": format!("http://{address}")}),
        )
        .await?;
        assert_eq!(connected.is_error, Some(false));
        let created = call(&client, "new_page", json!({"url": "example.test"})).await?;
        assert_eq!(created.is_error, Some(false), "{created:?}");
        let created = created.structured_content.expect("structured new_page");
        let page_id = created["pageId"].as_str().expect("公开页面 ID");
        assert!(page_id.starts_with("p_"));
        assert_ne!(page_id, "target-1");
        assert_eq!(created["url"], "https://example.test");
        assert_eq!(created["navigation"]["status"], "complete");

        let pages = call(&client, "list_pages", json!({})).await?;
        let pages = pages.structured_content.expect("structured list_pages");
        assert_eq!(pages["count"], 1);
        assert_eq!(pages["activePageId"], page_id);
        assert_eq!(pages["pages"][0]["hasController"], true);

        let snapshot = call(&client, "take_snapshot", json!({})).await?;
        assert_eq!(snapshot.is_error, Some(false));
        let snapshot = snapshot.structured_content.expect("structured snapshot");
        assert_eq!(snapshot["uidCount"], 1);
        assert_eq!(snapshot["nodeCount"], 2);
        let text = snapshot["snapshot"].as_str().expect("snapshot text");
        assert!(text.contains("RootWebArea"), "{text}");
        assert!(text.contains("button \"Sign in\""), "{text}");
        let uid = text
            .split_whitespace()
            .find_map(|token| token.strip_prefix("uid="))
            .expect("the button has a uid")
            .to_owned();

        let filled = call(
            &client,
            "fill",
            json!({"uid": uid.clone(), "text": "hello"}),
        )
        .await?;
        assert_eq!(filled.is_error, Some(false), "{filled:?}");
        let filled = filled.structured_content.expect("structured fill");
        assert_eq!(filled["length"], 5);
        assert_eq!(filled["cleared"], false);

        let cleared = call(&client, "fill", json!({"uid": uid.clone(), "text": ""})).await?;
        assert_eq!(cleared.is_error, Some(false), "{cleared:?}");
        let cleared = cleared.structured_content.expect("structured clear");
        assert_eq!(cleared["length"], 0);
        assert_eq!(cleared["cleared"], true);
        assert!(
            seen.lock().expect("seen lock").iter().any(|request| {
                request["method"] == "Input.dispatchKeyEvent"
                    && request["params"]["key"] == "Backspace"
            }),
            "empty fill must send a Backspace instead of a no-op insertText"
        );

        let clicked = call(&client, "click", json!({"uid": uid})).await?;
        assert_eq!(clicked.is_error, Some(false));
        let clicked = clicked.structured_content.expect("structured click");
        assert_eq!(clicked["backendNodeId"], 42);
        assert_eq!(clicked["x"], 20.0);
        assert_eq!(clicked["y"], 15.0);

        let navigated = call(
            &client,
            "navigate_page",
            json!({"url": "example.test/next"}),
        )
        .await?;
        assert_eq!(navigated.is_error, Some(false));
        let navigated = navigated.structured_content.expect("structured navigate");
        assert_eq!(navigated["url"], "https://example.test/next");
        assert_eq!(navigated["status"], "complete");

        let evaluated = call(&client, "evaluate_script", json!({"expression": "1 + 1"})).await?;
        assert_eq!(
            evaluated.structured_content.expect("structured eval")["result"]["value"],
            7
        );

        let shot = call(&client, "take_screenshot", json!({})).await?;
        assert_eq!(shot.is_error, Some(false));
        assert_eq!(
            shot.structured_content.expect("structured screenshot")["mimeType"],
            "image/jpeg"
        );
        assert!(
            shot.content
                .iter()
                .any(|block| matches!(block, ContentBlock::Image(_))),
            "the screenshot is an image block"
        );

        let pressed = call(&client, "press_key", json!({"key": "Control+A"})).await?;
        let pressed = pressed.structured_content.expect("structured press_key");
        assert_eq!(pressed["pressed"], "Control+A");
        assert_eq!(pressed["key"], "a");

        let waited = call(
            &client,
            "wait_for",
            json!({"text": "Sign in", "timeoutMs": 1000}),
        )
        .await?;
        assert_eq!(
            waited.structured_content.expect("structured wait")["matched"],
            true
        );

        let console = call(&client, "list_console_messages", json!({})).await?;
        assert_eq!(
            console.structured_content.expect("structured console")["count"],
            0
        );
        let network = call(&client, "list_network_requests", json!({})).await?;
        assert_eq!(
            network.structured_content.expect("structured network")["count"],
            0
        );

        let closed = call(&client, "close_page", json!({})).await?;
        assert_eq!(closed.is_error, Some(false));
        let closed = closed.structured_content.expect("structured close_page");
        assert_eq!(closed["closed"], page_id);
        assert_eq!(closed["count"], 0);
        assert!(closed["activePageId"].is_null());
        Ok(())
    })
    .await?;
    server.abort();
    Ok(())
}

async fn fake_cdp() -> (
    SocketAddr,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<Value>>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake CDP");
    let address = listener.local_addr().expect("fake CDP address");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server_seen = Arc::clone(&seen);
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_fake_cdp(stream, Arc::clone(&server_seen)));
        }
    });
    (address, server, seen)
}

/// The endpoint answers both surfaces on one port: a plain `GET` is the
/// `/json/version` probe, anything else is a WebSocket upgrade.
async fn serve_fake_cdp(stream: TcpStream, seen: Arc<Mutex<Vec<Value>>>) {
    // Both the JSON probes and the WebSocket upgrade start with `GET`; only
    // the upgrade advertises `Upgrade: websocket`.
    let mut prefix = [0_u8; 1024];
    let peeked = stream.peek(&mut prefix).await.unwrap_or(0);
    let head = String::from_utf8_lossy(&prefix[..peeked]).to_ascii_lowercase();
    if head.starts_with("get ") && !head.contains("upgrade: websocket") {
        serve_fake_probe(stream).await;
        return;
    }
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let mut url = "about:blank".to_owned();
    let mut targets = 0usize;
    while let Some(message) = socket.next().await {
        let text = match message {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => {
                let _ = socket.flush().await;
                break;
            }
            _ => break,
        };
        let Ok(request) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        seen.lock().expect("seen lock").push(request.clone());
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let result = fake_cdp_reply(method, &params, &mut url, &mut targets);
        let response = json!({"id": id, "result": result});
        if socket
            .send(Message::Text(response.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn serve_fake_probe(mut stream: TcpStream) {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                request.extend_from_slice(&chunk[..read]);
                if request
                    .windows(4)
                    .any(|window| window.starts_with(b"\r\n\r\n"))
                {
                    break;
                }
            }
        }
    }
    let text = String::from_utf8_lossy(&request);
    let path = text.split_whitespace().nth(1).unwrap_or_default();
    let body = match path {
        "/json/version" => json!({"Browser": "Kitesurf/0.5.4"}),
        "/json/list" => json!([{"id": "target-1", "type": "page"}]),
        _ => json!({"error": "not found"}),
    }
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn fake_cdp_reply(method: &str, params: &Value, url: &mut String, targets: &mut usize) -> Value {
    match method {
        "Target.createTarget" => {
            *targets += 1;
            json!({"targetId": format!("target-{targets}")})
        }
        "Runtime.evaluate" => {
            let expression = params
                .get("expression")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match expression {
                "location.href" => json!({"result": {"type": "string", "value": url.clone()}}),
                "document.title" => json!({"result": {"type": "string", "value": "Example"}}),
                "document.readyState" => {
                    json!({"result": {"type": "string", "value": "complete"}})
                }
                _ => {
                    let assigned = expression
                        .strip_prefix("location.assign(")
                        .and_then(|rest| rest.strip_suffix(')'));
                    if let Some(assigned) = assigned {
                        if let Ok(next) = serde_json::from_str::<String>(assigned) {
                            *url = next;
                        }
                        json!({"result": {"type": "object"}})
                    } else if expression.contains("document.body") {
                        json!({"result": {"type": "boolean", "value": true}})
                    } else {
                        json!({"result": {"type": "number", "value": 7}})
                    }
                }
            }
        }
        "Page.getFrameTree" => json!({"frameTree": {"frame": {"id": "top", "loaderId": url}}}),
        "Page.getNavigationHistory" => json!({
            "currentIndex": 0,
            "entries": [{"id": 1, "url": url.clone(), "title": "Example"}],
        }),
        "Accessibility.getFullAXTree" => serde_json::from_str(FAKE_AX_TREE).expect("valid tree"),
        "DOM.getBoxModel" => {
            json!({"model": {"content": [10.0, 10.0, 30.0, 10.0, 30.0, 20.0, 10.0, 20.0]}})
        }
        "Page.captureScreenshot" => json!({"data": "aGVsbG8="}),
        _ => json!({}),
    }
}
