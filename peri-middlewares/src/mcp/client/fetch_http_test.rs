//! 浏览器 fetch 通道的纯逻辑单测：状态分类、SSE 限流与分块解析、保留头校验。
//!
//! JS 交互（宿主 `fetch`、AbortController）无法在原生测试里复现，浏览器侧回归
//! 由人工验证覆盖；这里锁定的是与 rmcp reqwest 实现对线的语义。

use super::*;
use futures::StreamExt;
use rmcp::model::{ErrorCode, NumberOrString};

const MAX_EVENT: usize = 1024;

fn message(json: serde_json::Value) -> ClientJsonRpcMessage {
    serde_json::from_value(json).expect("测试消息应符合 JSON-RPC 形状")
}

fn request(method: &str) -> ClientJsonRpcMessage {
    message(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}}))
}

fn notification() -> ClientJsonRpcMessage {
    message(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
}

fn response(status: u16, content_type: Option<&str>, body: FetchBody) -> FetchResponse {
    FetchResponse {
        status,
        content_type: content_type.map(str::to_string),
        content_length: None,
        session_id: None,
        www_authenticate: None,
        body,
    }
}

fn classify(
    response: FetchResponse,
    message: &ClientJsonRpcMessage,
    session_was_attached: bool,
) -> Result<StreamableHttpPostResponse, StreamableHttpError<FetchHttpError>> {
    classify_post_response(response, message, session_was_attached, MAX_EVENT)
}

#[test]
fn accepted_statuses_short_circuit_the_body() {
    for status in [202u16, 204] {
        let result = classify(
            response(status, None, FetchBody::Text(String::new())),
            &notification(),
            false,
        );
        assert!(
            matches!(result, Ok(StreamableHttpPostResponse::Accepted)),
            "{status} 应判为 Accepted：{result:?}"
        );
    }
}

#[test]
fn auth_challenge_maps_to_auth_required() {
    let mut head = response(401, Some("text/plain"), FetchBody::Text("denied".into()));
    head.www_authenticate = Some("Bearer realm=\"mcp\"".into());
    let result = classify(head, &request("server/discover"), false);
    assert!(
        matches!(result, Err(StreamableHttpError::AuthRequired(_))),
        "401 带挑战头应上抛 AuthRequired：{result:?}"
    );
}

#[test]
fn forbidden_with_challenge_maps_to_insufficient_scope() {
    let mut head = response(403, None, FetchBody::Text(String::new()));
    head.www_authenticate =
        Some("Bearer error=\"insufficient_scope\", scope=\"tools:read\"".into());
    let result = classify(head, &request("tools/list"), false);
    assert!(
        matches!(result, Err(StreamableHttpError::InsufficientScope(_))),
        "403 带挑战头应上抛 InsufficientScope：{result:?}"
    );
}

#[test]
fn session_attached_404_is_expiry() {
    let result = classify(
        response(404, None, FetchBody::Text(String::new())),
        &request("tools/list"),
        true,
    );
    assert!(
        matches!(result, Err(StreamableHttpError::SessionExpired)),
        "带会话的 404 应判为会话过期：{result:?}"
    );
}

#[test]
fn empty_success_for_notification_is_accepted() {
    let result = classify(
        response(200, None, FetchBody::Text(String::new())),
        &notification(),
        false,
    );
    assert!(
        matches!(result, Ok(StreamableHttpPostResponse::Accepted)),
        "通知类 POST 的空 200 应判为 Accepted：{result:?}"
    );
}

#[test]
fn json_response_is_parsed_into_message() {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "result": {"resultType": "complete", "supportedVersions": ["2026-07-28"],
            "capabilities": {}, "serverInfo": {"name": "fixture", "version": "1"}}
    })
    .to_string();
    let result = classify(
        response(
            200,
            Some("application/json; charset=utf-8"),
            FetchBody::Text(body),
        ),
        &request("server/discover"),
        false,
    );
    assert!(
        matches!(result, Ok(StreamableHttpPostResponse::Json(_, None))),
        "JSON 正文应解析成一条消息：{result:?}"
    );
}

#[test]
fn discover_rejection_is_recorrelated_to_the_request_id() {
    let body = r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"}}"#;
    let result = classify(
        response(400, Some("application/json"), FetchBody::Text(body.into())),
        &request("server/discover"),
        false,
    );
    let Ok(StreamableHttpPostResponse::Json(ServerJsonRpcMessage::Error(error), session)) = result
    else {
        panic!("discover 的 4xx 应重关联成带 id 的 JSON-RPC 错误：{result:?}");
    };
    assert!(session.is_none());
    assert_eq!(error.id, Some(NumberOrString::Number(1)));
    assert_eq!(error.error.code, ErrorCode(-32601));
}

#[test]
fn non_discover_rejection_is_forwarded_without_id_recorrelation() {
    let body = r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"}}"#;
    let result = classify(
        response(400, Some("application/json"), FetchBody::Text(body.into())),
        &request("tools/list"),
        false,
    );
    let Ok(StreamableHttpPostResponse::Json(ServerJsonRpcMessage::Error(error), session)) = result
    else {
        panic!("非 2xx 的 JSON-RPC 错误正文应原样上报：{result:?}");
    };
    // 只有 discover 才补 id；其它请求保留响应里的原样形状。
    assert!(error.id.is_none());
    assert!(session.is_none());
}

#[test]
fn foreign_content_type_is_rejected() {
    let result = classify(
        response(
            200,
            Some("text/html"),
            FetchBody::Text("<html></html>".into()),
        ),
        &request("tools/list"),
        false,
    );
    match result {
        Err(StreamableHttpError::UnexpectedContentType(Some(content_type))) => {
            assert_eq!(content_type, "text/html");
        }
        other => panic!("外来内容类型应被拒绝：{other:?}"),
    }
}

#[test]
fn sse_body_is_parsed_event_by_event() {
    let (sender, receiver) = mpsc::channel(4);
    let result = classify(
        response(
            200,
            Some("text/event-stream"),
            FetchBody::Stream(FetchByteStream { receiver }),
        ),
        &request("tools/list"),
        false,
    );
    let Ok(StreamableHttpPostResponse::Sse(mut stream, _)) = result else {
        panic!("SSE 正文应给出事件流：{result:?}");
    };
    sender
        .try_send(Ok(Bytes::from_static(b"data: first\n\n")))
        .expect("通道有容量");
    sender
        .try_send(Ok(Bytes::from_static(b"event: tick\ndata: second\n\n")))
        .expect("通道有容量");
    let first = futures::executor::block_on(stream.next()).expect("流应有首个事件");
    let first = first.expect("首个事件应可解析");
    assert_eq!(first.data.as_deref(), Some("first"));
    let second = futures::executor::block_on(stream.next()).expect("流应有第二个事件");
    let second = second.expect("第二个事件应可解析");
    assert_eq!(second.event.as_deref(), Some("tick"));
    assert_eq!(second.data.as_deref(), Some("second"));
}

#[test]
fn oversized_sse_event_fails_the_stream() {
    let (sender, receiver) = mpsc::channel(4);
    let result = classify_post_response(
        response(
            200,
            Some("text/event-stream"),
            FetchBody::Stream(FetchByteStream { receiver }),
        ),
        &request("tools/list"),
        false,
        8,
    );
    let Ok(StreamableHttpPostResponse::Sse(mut stream, _)) = result else {
        panic!("SSE 正文应给出事件流：{result:?}");
    };
    sender
        .try_send(Ok(Bytes::from_static(b"data: 0123456789\n\n")))
        .expect("通道有容量");
    let error = futures::executor::block_on(stream.next())
        .expect("超限时流应给出终止项")
        .expect_err("超限事件应报错");
    assert!(
        matches!(error, SseError::Body(_)),
        "限流错误应经正文错误上抛：{error:?}"
    );
}

#[test]
fn limiter_resets_on_blank_line_and_handles_crlf() {
    // 单行 "data: ab" 连行结束符共 9 字节，预算 16：一个事件在限内，同一事件块里两行即超出。
    let mut limiter = SseEventSizeLimiter::new(16);
    // CRLF 只算一次行结束；若把 '\n' 当成第二个空行，计数会被错误归零，第二行就会漏过。
    assert!(limiter.observe(b"data: ab\r\n").is_ok());
    assert!(limiter.observe(b"data: cd\r\n").is_err());
    // 空行才是事件边界：归零后下一事件重新计量，同样落在限内。
    let mut limiter = SseEventSizeLimiter::new(16);
    assert!(limiter.observe(b"data: ab\r\n\r\n").is_ok());
    assert!(limiter.observe(b"data: cd\r\n").is_ok());
}

#[test]
fn reserved_headers_are_rejected_except_protocol_version() {
    for name in ["accept", "mcp-session-id", "Last-Event-Id"] {
        let name = HeaderName::from_bytes(name.as_bytes()).expect("测试头名合法");
        assert!(validate_custom_header(&name).is_err(), "{name} 应被拒绝");
    }
    for name in ["mcp-protocol-version", "authorization", "x-tenant"] {
        let name = HeaderName::from_bytes(name.as_bytes()).expect("测试头名合法");
        assert!(validate_custom_header(&name).is_ok(), "{name} 应放行");
    }
}

#[test]
fn request_headers_order_puts_accept_first_and_custom_last() {
    let mut custom = HashMap::new();
    custom.insert(
        HeaderName::from_static("x-tenant"),
        HeaderValue::from_static("acme"),
    );
    let headers = request_headers(Some("token"), Some("session-1"), Some("42"), &custom)
        .expect("保留头之外的请求头可组装");
    let names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "accept",
            "authorization",
            HEADER_SESSION_ID,
            HEADER_LAST_EVENT_ID,
            "x-tenant",
        ]
    );
    assert_eq!(headers[1].1, "Bearer token");
}

#[test]
fn header_lookup_is_case_insensitive_and_ignores_empty_values() {
    let mut headers = HashMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("content-length".to_string(), String::new());
    assert_eq!(
        find_header(&headers, "content-type").as_deref(),
        Some("application/json")
    );
    assert!(find_header(&headers, "content-length").is_none());
}
