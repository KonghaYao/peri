use std::time::Duration;

use axum::routing::get;
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use super::*;

#[test]
fn viewer_url_replaces_the_endpoint_path() {
    assert_eq!(
        page_viewer_url("ws://127.0.0.1:8827", "abc").unwrap(),
        "ws://127.0.0.1:8827/devtools/page/abc?role=viewer"
    );
    assert_eq!(
        page_viewer_url("ws://127.0.0.1:8827/devtools/browser", "abc").unwrap(),
        "ws://127.0.0.1:8827/devtools/page/abc?role=viewer"
    );
    assert_eq!(
        page_viewer_url("wss://example.test/devtools/browser", "abc.def-1_2").unwrap(),
        "wss://example.test/devtools/page/abc.def-1_2?role=viewer"
    );

    let error = page_viewer_url("ws://127.0.0.1:8827", "a/b").expect_err("path injection");
    assert!(error.to_string().contains("invalid target id"));
}

#[test]
fn viewer_url_rejects_non_websocket_endpoints() {
    let error = page_viewer_url("http://127.0.0.1:8827", "abc").expect_err("http is rejected");
    assert!(error.to_string().contains("ws:// or wss://"));

    let error = page_viewer_url("not a url", "abc").expect_err("garbage is rejected");
    assert!(error.to_string().contains("invalid CDP endpoint"));
}

#[test]
fn screencast_frame_decodes_base64_and_metadata() {
    let params = json!({
        "data": "aGVsbG8=",
        "sessionId": 7,
        "metadata": {
            "deviceWidth": 900,
            "deviceHeight": 600,
            "pageScaleFactor": 1.5,
            "scrollOffsetX": 4.0,
            "scrollOffsetY": 8.0
        }
    });

    let frame = ScreencastFrame::parse(&params).expect("valid frame");
    assert_eq!(frame.jpeg.as_ref(), b"hello");
    assert_eq!(frame.metadata.device_width, 900);
    assert_eq!(frame.metadata.device_height, 600);
    assert!((frame.metadata.page_scale_factor - 1.5).abs() < f64::EPSILON);
    assert!((frame.metadata.scroll_offset_x - 4.0).abs() < f64::EPSILON);
    assert_eq!(frame.session_id, json!(7));
}

#[test]
fn screencast_frame_rejects_bad_payloads() {
    let missing_data = json!({"metadata": {}});
    assert!(ScreencastFrame::parse(&missing_data).is_err());

    let bad_base64 = json!({"data": "%%%", "metadata": {"deviceWidth": 1, "deviceHeight": 1}});
    assert!(ScreencastFrame::parse(&bad_base64).is_err());

    let missing_metadata = json!({"data": "aGVsbG8="});
    assert!(ScreencastFrame::parse(&missing_metadata).is_err());
}

#[test]
fn metadata_defaults_keep_the_frame_usable() {
    let params = json!({
        "data": "aGVsbG8=",
        "metadata": {"deviceWidth": 0, "deviceHeight": 0}
    });
    let frame = ScreencastFrame::parse(&params).expect("defaults are accepted");
    assert!((frame.metadata.page_scale_factor - 1.0).abs() < f64::EPSILON);
    assert!((frame.metadata.scroll_offset_x).abs() < f64::EPSILON);
    assert_eq!(frame.session_id, Value::Null);
}

#[test]
fn viewport_prefers_css_metrics() {
    let metrics = json!({
        "layoutViewport": {"clientWidth": 800, "clientHeight": 600},
        "cssLayoutViewport": {"clientWidth": 1280, "clientHeight": 720}
    });
    assert_eq!(
        viewport_from_metrics(&metrics),
        Some(Viewport {
            width: 1280,
            height: 720
        })
    );

    assert_eq!(viewport_from_metrics(&json!({})), None);
    assert_eq!(
        viewport_from_metrics(&json!({"cssLayoutViewport": {"clientWidth": 0, "clientHeight": 0}})),
        None
    );
}

#[tokio::test]
async fn commands_resolve_and_events_stream_over_a_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("websocket handshake");

        while let Some(message) = socket.next().await {
            let Ok(Message::Text(text)) = message else {
                break;
            };
            let request: Value = serde_json::from_str(text.as_str()).expect("json command");
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let response = match method {
                "Page.getLayoutMetrics" => json!({
                    "id": id,
                    "result": {"cssLayoutViewport": {"clientWidth": 640, "clientHeight": 480}}
                }),
                "Page.startScreencast" => {
                    let event = json!({
                        "method": "Page.screencastFrame",
                        "params": {
                            "data": "aGVsbG8=",
                            "sessionId": 3,
                            "metadata": {"deviceWidth": 640, "deviceHeight": 480}
                        }
                    });
                    socket
                        .send(Message::Text(event.to_string().into()))
                        .await
                        .expect("event");
                    json!({"id": id, "result": {}})
                }
                "Page.screencastFrameAck" => json!({"id": id, "result": {}}),
                "Page.captureScreenshot" => {
                    json!({"id": id, "error": {"code": -32000, "message": "nope"}})
                }
                _ => json!({"id": id, "result": {}}),
            };
            socket
                .send(Message::Text(response.to_string().into()))
                .await
                .expect("response");
        }
    });

    let mut viewer = Viewer::connect(&format!("ws://{address}"), "t1")
        .await
        .expect("viewer connects");
    assert!(viewer.url().contains("/devtools/page/t1?role=viewer"));

    let metrics = viewer
        .command("Page.getLayoutMetrics", json!({}))
        .await
        .expect("metrics");
    assert_eq!(
        viewport_from_metrics(&metrics),
        Some(Viewport {
            width: 640,
            height: 480
        })
    );

    start_screencast(&viewer).await.expect("screencast starts");
    let event = tokio::time::timeout(Duration::from_secs(1), viewer.next_event())
        .await
        .expect("event arrives")
        .expect("event stream open");
    assert_eq!(event.method, "Page.screencastFrame");
    let frame = ScreencastFrame::parse(&event.params).expect("frame decodes");
    assert_eq!(frame.jpeg.as_ref(), b"hello");
    assert_eq!(frame.session_id, json!(3));

    let error = viewer
        .command("Page.captureScreenshot", json!({}))
        .await
        .expect_err("CDP error is surfaced");
    assert!(error.to_string().contains("CDP -32000: nope"));

    drop(viewer);
    server.abort();
}

#[tokio::test]
async fn a_closed_connection_fails_pending_commands() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("websocket handshake");
        let _ = socket.next().await;
        drop(socket);
    });

    let viewer = Viewer::connect(&format!("ws://{address}"), "t1")
        .await
        .expect("viewer connects");
    let error = viewer
        .command("Page.getLayoutMetrics", json!({}))
        .await
        .expect_err("closed socket is an error");
    assert!(error.to_string().contains("closed"), "{error}");

    server.abort();
}

#[tokio::test]
async fn capture_thumbnail_returns_the_base64_payload() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("handshake");
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let request: Value = serde_json::from_str(text.as_str()).expect("json");
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let response = json!({"id": id, "result": {"data": "dGh1bWI="}});
            socket
                .send(Message::Text(response.to_string().into()))
                .await
                .expect("response");
        }
    });

    let data = capture_thumbnail(&format!("ws://{address}"), "t1")
        .await
        .expect("thumbnail captured");
    assert_eq!(data, "dGh1bWI=");
    server.abort();
}

#[tokio::test]
async fn the_http_endpoint_is_not_used_as_a_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let app = Router::new().route("/", get(|| async { Json(json!({"ok": true})) }));
    let http = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let error = Viewer::connect(&format!("ws://{address}"), "t1")
        .await
        .expect_err("an HTTP server is not a viewer socket");
    let text = error.to_string();
    assert!(
        text.contains("cannot connect") || text.contains("closed") || text.contains("timed out"),
        "{text}"
    );
    http.abort();
}

#[tokio::test]
async fn a_flood_of_events_does_not_block_a_command_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("handshake");
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let request: Value = serde_json::from_str(text.as_str()).expect("json");
            if request.get("method").and_then(Value::as_str) == Some("Runtime.enable") {
                // Kitesurf replays every execution context of the target to
                // each session; more events than the buffer holds must not
                // stall the socket reader ahead of the reply.
                for index in 0..(EVENT_CAPACITY * 4) {
                    let event = json!({
                        "method": "Runtime.executionContextCreated",
                        "params": {"context": {"id": index}},
                    });
                    socket
                        .send(Message::Text(event.to_string().into()))
                        .await
                        .expect("event");
                }
                let reply = json!({"id": request["id"], "result": {}});
                socket
                    .send(Message::Text(reply.to_string().into()))
                    .await
                    .expect("reply");
            }
        }
    });

    let controller = Controller::connect(&format!("ws://{address}"), "t1")
        .await
        .expect("connect");
    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        controller.command("Runtime.enable", json!({})),
    )
    .await
    .expect("the reply must not wait behind the event queue");
    assert!(reply.is_ok(), "{reply:?}");
    server.abort();
}

#[test]
fn controller_url_is_the_writable_page_endpoint() {
    assert_eq!(
        page_controller_url("ws://127.0.0.1:8827", "abc", CONTROLLER_OWNER).unwrap(),
        "ws://127.0.0.1:8827/devtools/page/abc?owner=peri-browser-gateway"
    );
    // The viewer role is the one thing a controller must never carry: Kitesurf
    // would then refuse every `Input.*` command.
    assert!(
        !page_controller_url("ws://127.0.0.1:8827", "abc", CONTROLLER_OWNER)
            .unwrap()
            .contains("role=viewer")
    );

    let error =
        page_controller_url("ws://127.0.0.1:8827", "abc", "").expect_err("empty owner is rejected");
    assert!(error.to_string().contains("invalid controller owner"));

    let error = page_controller_url("ws://127.0.0.1:8827", "abc", "a b")
        .expect_err("owner with a space is rejected");
    assert!(error.to_string().contains("invalid controller owner"));

    let error = page_controller_url("ws://127.0.0.1:8827", "a/b", "owner")
        .expect_err("path injection is rejected");
    assert!(error.to_string().contains("invalid target id"));
}

/// Minimal CDP server that records requests and answers like Kitesurf does for
/// the controller helpers: `Runtime.evaluate` drives the page and the history
/// lags behind the committed document.
struct Recorder {
    url: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct FakePage {
    href: String,
    entries: Vec<(i64, String)>,
    current: usize,
}

async fn start_recorder() -> Recorder {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let mut page = FakePage {
        href: "https://example.test/".to_owned(),
        entries: vec![
            (7, "https://example.test/".to_owned()),
            (8, "https://example.test/next".to_owned()),
        ],
        current: 0,
    };
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("handshake");
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let request: Value = serde_json::from_str(text.as_str()).expect("json");
            recorded.lock().expect("request log").push(request.clone());
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let params = request.get("params").cloned().unwrap_or(Value::Null);
            let result = match method {
                "Page.getLayoutMetrics" => {
                    json!({"cssLayoutViewport": {"clientWidth": 800, "clientHeight": 600}})
                }
                "Page.getNavigationHistory" => json!({
                    "currentIndex": page.current,
                    "entries": page.entries.iter().map(|(id, url)| json!({
                        "id": id,
                        "url": url,
                        "title": "Example",
                    })).collect::<Vec<_>>(),
                }),
                "Runtime.evaluate" => evaluate(&mut page, params.get("expression").and_then(Value::as_str)),
                _ => json!({}),
            };
            let response = json!({"id": id, "result": result});
            socket
                .send(Message::Text(response.to_string().into()))
                .await
                .expect("response");
        }
    });
    Recorder {
        url: format!("ws://{address}"),
        requests,
        server,
    }
}

/// Answers the two expressions the helpers send. `location.assign` moves the
/// document but leaves the history where it was, like Kitesurf does.
fn evaluate(page: &mut FakePage, expression: Option<&str>) -> Value {
    let expression = expression.unwrap_or_default();
    if let Some(url) = expression
        .strip_prefix("location.assign(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        page.href = serde_json::from_str(url).expect("string literal");
        return json!({"result": {"type": "undefined"}});
    }
    match expression {
        "location.href" => json!({"result": {"value": page.href}}),
        "document.title" => json!({"result": {"value": "Example"}}),
        _ => json!({"result": {"value": "Example"}}),
    }
}

impl Recorder {
    fn methods(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("request log")
            .iter()
            .map(|request| {
                request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    fn params(&self, method: &str) -> Vec<Value> {
        self.requests
            .lock()
            .expect("request log")
            .iter()
            .filter(|request| request.get("method").and_then(Value::as_str) == Some(method))
            .map(|request| request.get("params").cloned().unwrap_or(Value::Null))
            .collect()
    }
}

#[tokio::test]
async fn input_helpers_send_the_cdp_input_domain() {
    let recorder = start_recorder().await;
    let controller = Controller::connect(&recorder.url, "t1")
        .await
        .expect("controller connects");
    assert!(controller.url().contains("/devtools/page/t1?owner="));
    assert_eq!(controller.endpoint(), controller.url());

    dispatch_mouse(
        &controller,
        MouseEventKind::Down,
        12.5,
        34.5,
        Some(MouseButton::Left),
        2,
        8,
    )
    .await
    .expect("mouse");
    dispatch_wheel(&controller, 1.0, 2.0, 0.0, -120.0)
        .await
        .expect("wheel");
    dispatch_key(&controller, KeyEventKind::Down, "a", "KeyA", Some("a"), 2)
        .await
        .expect("key down");
    dispatch_key(&controller, KeyEventKind::Up, "a", "KeyA", None, 2)
        .await
        .expect("key up");
    insert_text(&controller, "héllo").await.expect("text");

    assert_eq!(
        recorder.params("Input.dispatchMouseEvent"),
        vec![json!({
            "type": "mousePressed",
            "x": 12.5,
            "y": 34.5,
            "button": "left",
            "buttons": 1,
            "clickCount": 2,
            "modifiers": 8
        })]
    );
    assert_eq!(
        recorder.params("Input.dispatchMouseWheel"),
        vec![json!({"type": "mouseWheel", "x": 1.0, "y": 2.0, "deltaX": 0.0, "deltaY": -120.0})]
    );
    let keys = recorder.params("Input.dispatchKeyEvent");
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0]["type"], "keyDown");
    assert_eq!(keys[0]["key"], "a");
    assert_eq!(keys[0]["text"], "a");
    assert_eq!(keys[0]["windowsVirtualKeyCode"], 65);
    assert_eq!(keys[1]["type"], "keyUp");
    assert!(keys[1].get("text").is_none());
    assert_eq!(
        recorder.params("Input.insertText"),
        vec![json!({"text": "héllo"})]
    );

    recorder.server.abort();
}

#[tokio::test]
async fn navigation_helpers_read_and_drive_the_page() {
    let recorder = start_recorder().await;
    let controller = Controller::connect(&recorder.url, "t1")
        .await
        .expect("controller connects");

    let state = navigation_state(&controller).await.expect("navigation");
    assert_eq!(state.url, "https://example.test/");
    assert_eq!(state.title, "Example");
    assert!(!state.can_go_back);
    assert!(state.can_go_forward);

    let state = navigate(&controller, "https://example.test/next")
        .await
        .expect("navigate");
    // The history still points at the old document; the live location wins.
    assert_eq!(state.url, "https://example.test/next");
    assert!(state.can_go_back);
    assert!(!state.can_go_forward);
    assert!(
        recorder.params("Page.navigate").is_empty(),
        "Page.navigate wedges Kitesurf, so it must not be used"
    );
    assert!(recorder.params("Runtime.evaluate").iter().any(|params| {
        params["expression"] == json!("location.assign(\"https://example.test/next\")")
    }));

    enable_diagnostics(&controller).await.expect("diagnostics");
    for domain in ["Page.enable", "Runtime.enable", "Network.enable"] {
        assert_eq!(
            recorder
                .methods()
                .iter()
                .filter(|method| *method == domain)
                .count(),
            1,
            "{domain} must be enabled once"
        );
    }

    let viewport = layout_metrics(&controller).await.expect("metrics");
    assert_eq!(
        viewport,
        Some(Viewport {
            width: 800,
            height: 600
        })
    );

    recorder.server.abort();
}

#[test]
fn address_bar_input_gets_the_scheme_a_browser_would_pick() {
    assert_eq!(normalize_url("example.com"), "https://example.com");
    assert_eq!(
        normalize_url("  example.com/a?b=1  "),
        "https://example.com/a?b=1"
    );
    // A port is not a scheme.
    assert_eq!(normalize_url("example.com:8443"), "https://example.com:8443");
    assert_eq!(normalize_url("localhost:7391"), "http://localhost:7391");
    assert_eq!(
        normalize_url("127.0.0.1:8827/json"),
        "http://127.0.0.1:8827/json"
    );
    assert_eq!(normalize_url("http://example.com"), "http://example.com");
    assert_eq!(normalize_url("https://example.com"), "https://example.com");
    assert_eq!(normalize_url("about:blank"), "about:blank");
    assert_eq!(normalize_url("data:text/html,hi"), "data:text/html,hi");
}

#[test]
fn virtual_key_codes_cover_the_monitor_keys() {
    assert_eq!(virtual_key_code("a", "KeyA"), Some(65));
    assert_eq!(virtual_key_code("z", "KeyZ"), Some(90));
    assert_eq!(virtual_key_code("5", "Digit5"), Some(53));
    assert_eq!(virtual_key_code("5", "Numpad5"), Some(101));
    assert_eq!(virtual_key_code("Enter", ""), Some(13));
    assert_eq!(virtual_key_code("ArrowLeft", ""), Some(37));
    assert_eq!(virtual_key_code("Backspace", ""), Some(8));
    assert_eq!(virtual_key_code("Unidentified", ""), None);
}
