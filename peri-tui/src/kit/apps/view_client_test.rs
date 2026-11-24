use super::*;
use crate::kit::apps::mapping::ViewportPoint;
use crate::kit::apps::test_support::{StubServer, http_response, sse_response};
use serde_json::json;
use std::time::Duration;

fn client(url: &str, token: &str) -> AppsViewClient {
    AppsViewClient::new(url, ViewToken::new(token).expect("token")).expect("client")
}

#[tokio::test]
async fn create_session_sends_html_body_and_bearer_token() {
    let server = StubServer::start(|request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/view/sessions");
        assert_eq!(request.header("authorization"), Some("Bearer secret-token"));
        assert_eq!(
            request.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(request.body, b"<html>app</html>");
        http_response(
            201,
            "application/json",
            &json!({
                "sessionId": "view-1",
                "url": "http://127.0.0.1:7392/apps/view?view=view-1",
                "eventsUrl": "/view/sessions/view-1/events",
                "state": "starting",
            })
            .to_string()
            .into_bytes(),
        )
    })
    .await;
    let client = client(server.url(), "secret-token");
    let created = client
        .create_session("<html>app</html>")
        .await
        .expect("created");
    assert_eq!(created.session_id, "view-1");
    assert_eq!(created.events_url, "/view/sessions/view-1/events");
    assert_eq!(
        server.wait_requests(1, Duration::from_secs(1)).await.len(),
        1
    );
}

#[tokio::test]
async fn http_error_keeps_only_the_code() {
    let server = StubServer::start(|_| {
        http_response(
            413,
            "application/json",
            br#"{"code":"html_too_large","message":"at most 5 MiB"}"#,
        )
    })
    .await;
    let error = client(server.url(), "secret-token")
        .create_session("<html></html>")
        .await
        .expect_err("must fail");
    assert_eq!(error.code(), "html_too_large");
    assert_eq!(error.status(), Some(413));
    let text = error.to_string();
    assert!(text.contains("html_too_large"));
    assert!(!text.contains("secret-token"));
    assert!(!text.contains(server.url()));
}

#[tokio::test]
async fn unauthorized_body_code_is_used_without_echoing_secrets() {
    let server = StubServer::start(|_| {
        http_response(
            401,
            "application/json",
            br#"{"code":"token_required","message":"Authorization: Bearer nope"}"#,
        )
    })
    .await;
    let error = client(server.url(), "secret-token")
        .delete_session("view-1")
        .await
        .expect_err("must fail");
    assert_eq!(error.code(), "token_required");
    assert!(!error.to_string().contains("Bearer"));
    assert!(!error.to_string().contains("secret-token"));
}

#[tokio::test]
async fn delete_is_idempotent_for_unknown_sessions() {
    let server = StubServer::start(|request| {
        assert_eq!(request.method, "DELETE");
        assert_eq!(request.path, "/view/sessions/gone");
        http_response(404, "application/json", br#"{"code":"session_not_found"}"#)
    })
    .await;
    client(server.url(), "token")
        .delete_session("gone")
        .await
        .expect("404 is fine");
}

#[tokio::test]
async fn oversized_html_is_rejected_before_any_request() {
    let server = StubServer::start(|_| http_response(201, "application/json", b"{}")).await;
    let client = client(server.url(), "token");
    let oversized = "x".repeat(MAX_VIEW_HTML_BYTES + 1);
    let error = client
        .create_session(&oversized)
        .await
        .expect_err("too large");
    assert_eq!(error.code(), "html_too_large");
    assert!(server.requests().is_empty(), "5 MiB 上限必须在发请求前拒绝");
}

#[tokio::test]
async fn sse_stream_decodes_meta_state_frame_and_closed() {
    let frame_bytes = b"fake-jpeg";
    let frame_b64 = base64::engine::general_purpose::STANDARD.encode(frame_bytes);
    let wire = format!(
        ": keepalive\n\n\
         event: meta\ndata: {{\"sessionId\":\"view-1\",\"state\":\"starting\",\"frames\":0,\"pendingRequests\":0}}\n\n\
         event: state\ndata: {{\"state\":\"ready\"}}\n\n\
         event: frame\ndata: {{\"seq\":7,\"width\":1280,\"height\":720,\"viewportWidth\":1280,\"viewportHeight\":720,\"format\":\"jpeg\",\"data\":\"{frame_b64}\"}}\n\n\
         event: app-request\ndata: {{\"seq\":1,\"id\":\"rpc-1\",\"method\":\"tools/call\",\"params\":{{\"name\":\"get-time\"}}}}\n\n\
         event: closed\ndata: {{\"reason\":\"deleted\"}}\n\n"
    );
    let server = StubServer::start(move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/view/sessions/view-1/events");
        assert_eq!(request.header("authorization"), Some("Bearer token"));
        assert_eq!(request.header("accept"), Some("text/event-stream"));
        sse_response(&wire)
    })
    .await;
    let client = client(server.url(), "token");
    let mut stream = client
        .connect_events("view-1", Some("/view/sessions/view-1/events"))
        .await
        .expect("stream");

    let meta = stream.next_event().await.expect("meta").expect("event");
    assert!(matches!(meta, ViewEvent::Meta(status) if status.state == "starting"));
    let state = stream.next_event().await.expect("state").expect("event");
    assert!(matches!(state, ViewEvent::Status(status) if status.state == "ready"));
    let frame = stream.next_event().await.expect("frame").expect("event");
    let ViewEvent::Frame(frame) = frame else {
        panic!("expected frame");
    };
    assert_eq!(frame.seq, 7);
    assert_eq!(frame.width, 1280);
    assert_eq!(decode_frame_bytes(&frame).expect("base64"), frame_bytes);
    let request = stream.next_event().await.expect("request").expect("event");
    let ViewEvent::AppRequest(request) = request else {
        panic!("expected app-request");
    };
    assert_eq!(request.method, "tools/call");
    let closed = stream.next_event().await.expect("closed").expect("event");
    assert_eq!(
        closed,
        ViewEvent::Closed {
            reason: Some("deleted".to_string())
        }
    );
    assert!(stream.next_event().await.expect("eof").is_none());
}

#[tokio::test]
async fn sse_stream_reconnects_by_opening_a_new_http_request() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_handler = std::sync::Arc::clone(&calls);
    let server = StubServer::start(move |_| {
        let call = calls_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            sse_response("event: state\ndata: {\"state\":\"ready\"}\n\n")
        } else {
            sse_response("event: closed\ndata: {\"reason\":null}\n\n")
        }
    })
    .await;
    let client = client(server.url(), "token");
    let mut first = client.connect_events("view-1", None).await.expect("first");
    assert!(matches!(
        first.next_event().await.expect("state"),
        Some(ViewEvent::Status(_))
    ));
    assert!(first.next_event().await.expect("eof").is_none());
    let mut second = client.connect_events("view-1", None).await.expect("second");
    assert!(matches!(
        second.next_event().await.expect("closed"),
        Some(ViewEvent::Closed { .. })
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn decode_event_ignores_unknown_and_malformed_payloads() {
    assert!(
        decode_event(&RawSseEvent {
            event: "unknown".to_string(),
            data: "{}".to_string(),
        })
        .is_none()
    );
    assert!(
        decode_event(&RawSseEvent {
            event: "frame".to_string(),
            data: "{\"seq\":1}".to_string(),
        })
        .is_none(),
        "缺字段/非 jpeg 的帧不投递"
    );
    // 非法错误码被丢弃（保留 failed 事实，但不让任意文本进入 UI）。
    let failed = decode_event(&RawSseEvent {
        event: "state".to_string(),
        data: "{\"state\":\"failed\",\"code\":\"bad code!\"}".to_string(),
    });
    assert!(matches!(
        failed,
        Some(ViewEvent::Status(status)) if status.code.is_none()
    ));
}

#[test]
fn error_display_contains_only_code_and_status() {
    let error = ViewClientError::with_status(500, br#"{"code":"boom","message":"secret detail"}"#);
    assert_eq!(error.to_string(), "boom (HTTP 500)");
    assert!(!error.to_string().contains("secret detail"));
}

#[test]
fn frame_debug_redacts_payload() {
    let frame = FrameEvent {
        seq: 9,
        width: 10,
        height: 20,
        viewport_width: 10,
        viewport_height: 20,
        data_base64: "c2VjcmV0LXBheWxvYWQ=".to_string(),
    };
    let debug = format!("{frame:?}");
    assert!(!debug.contains("c2VjcmV0"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
    assert!(debug.contains("seq: 9"), "{debug}");
}

#[tokio::test]
async fn absolute_events_url_must_keep_the_configured_origin() {
    let server = StubServer::start(|_| http_response(200, "text/event-stream", b"")).await;
    let client = client(server.url(), "token");
    let error = client
        .connect_events(
            "view-1",
            Some("http://127.0.0.1:1/view/sessions/view-1/events"),
        )
        .await
        .expect_err("cross-origin events url");
    assert_eq!(error.code(), "events_url_origin_mismatch");
    assert!(
        server.requests().is_empty(),
        "拒绝时不得发出携带 token 的请求"
    );
}

#[tokio::test]
async fn click_posts_exact_css_point_with_bearer_token() {
    let server = StubServer::start(|request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/view/sessions/view-1/click");
        assert_eq!(request.header("authorization"), Some("Bearer secret-token"));
        let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
        assert_eq!(
            body,
            json!({"action": "click", "x": 12.5, "y": 34.0}),
            "click body 必须与网关 ClickBody 完全一致"
        );
        http_response(
            200,
            "application/json",
            br#"{"clicked":true,"x":12.5,"y":34.0,"sessionId":"view-1"}"#,
        )
    })
    .await;
    client(server.url(), "secret-token")
        .click("view-1", ViewportPoint { x: 12.5, y: 34.0 })
        .await
        .expect("click");
    assert_eq!(
        server.wait_requests(1, Duration::from_secs(1)).await.len(),
        1
    );
}

#[tokio::test]
async fn click_rejects_non_finite_points_without_a_request() {
    let server = StubServer::start(|_| http_response(200, "application/json", b"{}")).await;
    let error = client(server.url(), "token")
        .click(
            "view-1",
            ViewportPoint {
                x: f64::NAN,
                y: 1.0,
            },
        )
        .await
        .expect_err("nan");
    assert_eq!(error.code(), "invalid_point");
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn app_result_posts_raw_result_or_error_exactly_once() {
    let server = StubServer::start(|request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/view/sessions/view-1/app-results");
        assert_eq!(request.header("authorization"), Some("Bearer token"));
        let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
        let has_result = body.get("result").is_some();
        let has_error = body.get("error").is_some();
        assert!(
            has_result != has_error,
            "AppResultBody 必须恰好一个 result/error"
        );
        assert_eq!(body["seq"], json!(7));
        if has_result {
            assert_eq!(
                body["result"],
                json!({"content": [{"type": "text", "text": "ok"}]})
            );
        } else {
            assert_eq!(body["error"], json!({"code": -32603, "message": "boom"}));
        }
        http_response(
            200,
            "application/json",
            br#"{"delivered":true,"seq":7,"sessionId":"view-1"}"#,
        )
    })
    .await;
    let client = client(server.url(), "token");
    client
        .app_result(
            "view-1",
            7,
            ViewAppResult::Result(json!({"content": [{"type": "text", "text": "ok"}]})),
        )
        .await
        .expect("result");
    client
        .app_result(
            "view-1",
            7,
            ViewAppResult::Error {
                code: -32603,
                message: "boom".to_string(),
            },
        )
        .await
        .expect("error");
    assert_eq!(
        server.wait_requests(2, Duration::from_secs(1)).await.len(),
        2
    );
}

#[tokio::test]
async fn click_error_maps_status_and_keeps_secrets_out() {
    let server = StubServer::start(|_| {
        http_response(
            409,
            "application/json",
            br#"{"error":"session_starting","message":"wait for ready"}"#,
        )
    })
    .await;
    let error = client(server.url(), "secret-token")
        .click("view-1", ViewportPoint { x: 1.0, y: 2.0 })
        .await
        .expect_err("conflict");
    assert_eq!(error.code(), "session_starting");
    assert!(!error.to_string().contains("secret-token"));
    assert!(!error.to_string().contains("wait for ready"));
}
