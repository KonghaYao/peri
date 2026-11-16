use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, NumberOrString, PingRequest};
use rmcp::transport::auth::{AuthClient, AuthorizationManager};
use rmcp::transport::streamable_http_client::{StreamableHttpClient, StreamableHttpPostResponse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use super::PeriHttpClient;

fn ping() -> ClientJsonRpcMessage {
    ClientJsonRpcMessage::request(
        ClientRequest::PingRequest(PingRequest::default()),
        NumberOrString::Number(1),
    )
}

async fn spawn_server(responses: Vec<Vec<u8>>) -> (Arc<str>, JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            socket.write_all(&response).await.unwrap();
            socket.shutdown().await.unwrap();
            requests.push(request);
        }
        requests
    });
    (format!("http://{addr}/mcp").into(), task)
}

async fn read_request(socket: &mut TcpStream) -> Vec<u8> {
    let mut data = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = socket.read(&mut chunk).await.unwrap();
        assert!(read > 0, "connection closed before headers");
        data.extend_from_slice(&chunk[..read]);
        if let Some(position) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..header_end]).to_ascii_lowercase();
    let content_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map(|value| value.trim().parse::<usize>().unwrap())
        .unwrap_or(0);
    while data.len() < header_end + content_length {
        let read = socket.read(&mut chunk).await.unwrap();
        if read == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..read]);
    }
    data
}

fn response(status: &str, headers: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn accepted() -> Vec<u8> {
    response("202 Accepted", "", "")
}

fn oversized_sse() -> Vec<u8> {
    response(
        "200 OK",
        "Content-Type: text/event-stream\r\n",
        "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n",
    )
}

fn request_text(request: &[u8]) -> String {
    String::from_utf8_lossy(request).to_ascii_lowercase()
}

#[tokio::test]
async fn plain_post_forwards_session_auth_and_custom_headers() {
    let (uri, server) = spawn_server(vec![accepted()]).await;
    let client = PeriHttpClient::Plain(reqwest::Client::new());
    let mut custom = HashMap::new();
    custom.insert(
        HeaderName::from_static("x-test"),
        HeaderValue::from_static("yes"),
    );
    let response = client
        .post_message(
            uri,
            ping(),
            Some(Arc::from("session-1")),
            Some("token-1".to_string()),
            custom,
        )
        .await
        .unwrap();
    assert!(matches!(response, StreamableHttpPostResponse::Accepted));

    let request = request_text(&server.await.unwrap()[0]);
    assert!(request.starts_with("post /mcp "), "{request}");
    assert!(request.contains("\r\nx-test: yes"), "{request}");
    assert!(
        request.contains("\r\nmcp-session-id: session-1"),
        "{request}"
    );
    assert!(
        request.contains("\r\nauthorization: bearer token-1"),
        "{request}"
    );
}

#[tokio::test]
async fn authed_post_goes_out_unauthenticated_without_credentials() {
    let (uri, server) = spawn_server(vec![accepted()]).await;
    let manager = AuthorizationManager::new(uri.to_string()).await.unwrap();
    let client = PeriHttpClient::Authed(AuthClient::new(reqwest::Client::new(), manager));
    let response = client
        .post_message(uri, ping(), None, None, HashMap::new())
        .await
        .unwrap();
    assert!(matches!(response, StreamableHttpPostResponse::Accepted));

    let request = request_text(&server.await.unwrap()[0]);
    assert!(!request.contains("\r\nauthorization:"), "{request}");
}

#[tokio::test]
async fn authed_post_preserves_401_challenge() {
    let (uri, server) = spawn_server(vec![response(
        "401 Unauthorized",
        "WWW-Authenticate: Bearer resource_metadata=\"http://127.0.0.1/auth\"\r\n",
        "",
    )])
    .await;
    let manager = AuthorizationManager::new(uri.to_string()).await.unwrap();
    let client = PeriHttpClient::Authed(AuthClient::new(reqwest::Client::new(), manager));
    let error = client
        .post_message(uri, ping(), None, None, HashMap::new())
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            rmcp::transport::streamable_http_client::StreamableHttpError::AuthRequired(_)
        ),
        "{error:?}"
    );
    server.await.unwrap();
}

async fn assert_post_sse_limit_is_forwarded(client: PeriHttpClient) {
    let (uri, server) = spawn_server(vec![oversized_sse()]).await;
    let response = client
        .post_message_with_max_sse_event_size(uri, ping(), None, None, HashMap::new(), 8)
        .await
        .unwrap();
    let StreamableHttpPostResponse::Sse(mut stream, _) = response else {
        panic!("expected SSE response");
    };
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(
        error.to_string().contains("maximum size of 8 bytes"),
        "{error}"
    );
    server.await.unwrap();
}

async fn authed_client() -> PeriHttpClient {
    let manager = AuthorizationManager::new("http://127.0.0.1:1/mcp")
        .await
        .unwrap();
    PeriHttpClient::Authed(AuthClient::new(reqwest::Client::new(), manager))
}

#[tokio::test]
async fn plain_forwards_post_sse_limit() {
    assert_post_sse_limit_is_forwarded(PeriHttpClient::Plain(reqwest::Client::new())).await;
}

#[tokio::test]
async fn authed_forwards_post_sse_limit() {
    assert_post_sse_limit_is_forwarded(authed_client().await).await;
}

async fn assert_get_stream_limit_is_forwarded(client: PeriHttpClient) {
    let (uri, server) = spawn_server(vec![oversized_sse()]).await;
    let mut stream = client
        .get_stream_with_max_sse_event_size(
            uri,
            None,
            Some("last-1".to_string()),
            None,
            HashMap::new(),
            8,
        )
        .await
        .unwrap();
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(
        error.to_string().contains("maximum size of 8 bytes"),
        "{error}"
    );

    let request = request_text(&server.await.unwrap()[0]);
    assert!(request.starts_with("get /mcp "), "{request}");
    assert!(request.contains("\r\nlast-event-id: last-1"), "{request}");
}

#[tokio::test]
async fn plain_forwards_get_stream_limit_and_last_event_id() {
    assert_get_stream_limit_is_forwarded(PeriHttpClient::Plain(reqwest::Client::new())).await;
}

#[tokio::test]
async fn authed_forwards_get_stream_limit_and_last_event_id() {
    assert_get_stream_limit_is_forwarded(authed_client().await).await;
}

#[tokio::test]
async fn plain_delete_sends_session_header() {
    let (uri, server) = spawn_server(vec![response("204 No Content", "", "")]).await;
    let client = PeriHttpClient::Plain(reqwest::Client::new());
    client
        .delete_session(uri, Arc::from("session-1"), None, HashMap::new())
        .await
        .unwrap();
    let request = request_text(&server.await.unwrap()[0]);
    assert!(request.starts_with("delete /mcp "), "{request}");
    assert!(
        request.contains("\r\nmcp-session-id: session-1"),
        "{request}"
    );
}
