use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

async fn read_mcp_request(socket: &mut TcpStream) -> Option<serde_json::Value> {
    let mut request = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let size = socket.read(&mut chunk).await.ok()?;
        if size == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..size]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let request_line = headers.lines().next()?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next()?;
    let path = request_parts.next()?;
    if method != "POST" || path != "/mcp" {
        let (status, extra_headers) = if method == "GET" && path == "/mcp" {
            ("405 Method Not Allowed", "Allow: POST\r\n")
        } else {
            ("404 Not Found", "")
        };
        let response = format!(
            "HTTP/1.1 {status}\r\n{extra_headers}Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        socket.write_all(response.as_bytes()).await.ok()?;
        return None;
    }
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let size = socket.read(&mut chunk).await.ok()?;
        if size == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..size]);
    }
    serde_json::from_slice(&request[header_end..header_end + content_length]).ok()
}

async fn handle_mcp_request(
    mut socket: TcpStream,
    listen_requests: Arc<AtomicUsize>,
    cancel_requests: Arc<AtomicUsize>,
) {
    let Some(message) = read_mcp_request(&mut socket).await else {
        return;
    };
    let method = message["method"].as_str().unwrap_or_default();
    if method == "subscriptions/listen" {
        listen_requests.fetch_add(1, Ordering::SeqCst);
        std::future::pending::<()>().await;
        return;
    }
    if method == "tasks/cancel" {
        // An owner that accepts the connection but never settles the cancel.
        cancel_requests.fetch_add(1, Ordering::SeqCst);
        std::future::pending::<()>().await;
        return;
    }
    if message.get("id").is_none() {
        let _ = socket
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    }

    let response_body = match method {
        "server/discover" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "resultType": "complete",
                "supportedVersions": ["2026-07-28"],
                "capabilities": {"tools": {}, "resources": {"subscribe": true}},
                "serverInfo": {"name": "unresponsive-subscription-fixture", "version": "1.0.0"},
                "ttlMs": 0,
                "cacheScope": "private"
            }
        }),
        "initialize" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "protocolVersion": "2026-07-28",
                "capabilities": {"tools": {}, "resources": {"subscribe": true}},
                "serverInfo": {"name": "unresponsive-subscription-fixture", "version": "1.0.0"}
            }
        }),
        "tools/list" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {"tools": [{
                    "name": "fixture-tool",
                    "description": "Fixture tool",
                    "inputSchema": {"type": "object", "properties": {}}
                }]}
        }),
        "resources/list" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {"resources": []}
        }),
        "ping" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {}
        }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "error": {"code": -32601, "message": "Method not found"}
        }),
    };
    let body = response_body.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = socket.write_all(response.as_bytes()).await;
}

async fn serve_unresponsive_subscription_server(
    listener: TcpListener,
    listen_requests: Arc<AtomicUsize>,
    cancel_requests: Arc<AtomicUsize>,
) {
    let mut requests = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else {
                    return;
                };
                requests.spawn(handle_mcp_request(
                    socket,
                    listen_requests.clone(),
                    cancel_requests.clone(),
                ));
            }
            Some(_) = requests.join_next(), if !requests.is_empty() => {}
        }
    }
}

fn subscription_config(url: String) -> super::super::config::McpConfigFile {
    serde_json::from_value(serde_json::json!({
        "mcpServers": {
            "server": {
                "url": url,
                "subscriptions": {"resources": ["resource://fixture"]}
            }
        }
    }))
    .unwrap()
}

async fn start_subscription_server() -> (
    String,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let listen_requests = Arc::new(AtomicUsize::new(0));
    let cancel_requests = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn(serve_unresponsive_subscription_server(
        listener,
        listen_requests.clone(),
        cancel_requests.clone(),
    ));
    (url, listen_requests, cancel_requests, server)
}

async fn initialize_pool(url: String) -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, _) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        Path::new("."),
        subscription_config(url),
        Default::default(),
        status_tx,
        None,
    )
    .await;
    pool
}

fn assert_connected_with_fixture_tool(pool: &McpClientPool) {
    let handle = pool.get_client("server").unwrap();
    assert!(matches!(&handle.status, ClientStatus::Connected));
    assert_eq!(
        handle
            .tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        vec!["fixture-tool"]
    );
}

async fn wait_for_listen_request(listen_requests: &AtomicUsize, expected_count: usize) {
    for _ in 0..5_000 {
        if listen_requests.load(Ordering::SeqCst) >= expected_count {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        listen_requests.load(Ordering::SeqCst) >= expected_count,
        "fixture server did not receive the subscriptions/listen request"
    );
}

async fn wait_for_cancel_request(cancel_requests: &AtomicUsize, expected_count: usize) {
    for _ in 0..5_000 {
        if cancel_requests.load(Ordering::SeqCst) >= expected_count {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        cancel_requests.load(Ordering::SeqCst) >= expected_count,
        "fixture server did not receive the tasks/cancel request"
    );
}

async fn wait_for_task<T>(task: tokio::task::JoinHandle<T>, description: &str) -> T {
    for _ in 0..5_000 {
        if task.is_finished() {
            return task.await.expect(description);
        }
        tokio::task::yield_now().await;
    }
    panic!("{description}");
}

#[tokio::test(start_paused = true)]
async fn initialize_continues_after_unanswered_subscription_handshake() {
    let timeout = super::super::client::SUBSCRIPTION_LISTEN_TIMEOUT;
    let (url, listen_requests, _cancel_requests, server) = start_subscription_server().await;
    let initialization = tokio::spawn(async move { initialize_pool(url).await });
    wait_for_listen_request(&listen_requests, 1).await;
    tokio::time::advance(timeout).await;
    let pool = wait_for_task(initialization, "initialization task should finish").await;
    assert_connected_with_fixture_tool(&pool);
    assert_eq!(listen_requests.load(Ordering::SeqCst), 1);

    let _ = pool.shutdown().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test(start_paused = true)]
async fn reconnect_continues_after_unanswered_subscription_handshake() {
    let timeout = super::super::client::SUBSCRIPTION_LISTEN_TIMEOUT;
    let (url, listen_requests, _cancel_requests, server) = start_subscription_server().await;
    let initialization = tokio::spawn(async move { initialize_pool(url).await });
    wait_for_listen_request(&listen_requests, 1).await;
    tokio::time::advance(timeout).await;
    let pool = wait_for_task(initialization, "initialization task should finish").await;
    assert_connected_with_fixture_tool(&pool);
    assert_eq!(listen_requests.load(Ordering::SeqCst), 1);

    let reconnect_pool = Arc::clone(&pool);
    let reconnect = tokio::spawn(async move { reconnect_pool.reconnect("server", None).await });
    wait_for_listen_request(&listen_requests, 2).await;
    tokio::time::advance(timeout).await;
    wait_for_task(reconnect, "reconnect task should finish")
        .await
        .expect("connection remains usable after subscription timeout");
    assert_connected_with_fixture_tool(&pool);
    assert_eq!(listen_requests.load(Ordering::SeqCst), 2);

    let _ = pool.shutdown().await;
    server.abort();
    let _ = server.await;
}

/// A connected owner that accepts `tasks/cancel` but never replies must not
/// hold the cancellation future forever.
#[tokio::test(start_paused = true)]
async fn cancel_task_rpc_returns_after_its_own_deadline() {
    use peri_acp_types::tasks::{BgTaskKind, TaskManager};

    let listen_timeout = super::super::client::SUBSCRIPTION_LISTEN_TIMEOUT;
    let cancel_timeout = super::super::client::MCP_TASK_CANCEL_TIMEOUT;
    let (url, listen_requests, cancel_requests, server) = start_subscription_server().await;
    let initialization = tokio::spawn(async move { initialize_pool(url).await });
    wait_for_listen_request(&listen_requests, 1).await;
    tokio::time::advance(listen_timeout).await;
    let pool = wait_for_task(initialization, "initialization task should finish").await;
    assert_connected_with_fixture_tool(&pool);

    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    pool.bind_session_task_manager("session", &manager);
    let ticket = pool
        .register_external_task(
            "session",
            Some("session"),
            None,
            "server",
            "remote-task",
            BgTaskKind::Mcp,
            "cancel fixture",
            false,
            "2026-10-24T00:00:00Z",
        )
        .unwrap();

    let cancellation = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move { manager.cancel_async(ticket.task_id()).await }
    });
    wait_for_cancel_request(&cancel_requests, 1).await;
    tokio::time::advance(cancel_timeout).await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), cancellation)
        .await
        .expect("tasks/cancel must return after its own deadline")
        .expect("cancellation task must not panic");
    assert!(
        result.is_err(),
        "an unanswered tasks/cancel must fail instead of hanging"
    );
    assert!(
        manager.has_unsettled_external(),
        "a cancel timeout must not settle the task"
    );
    assert_eq!(
        manager.snapshot().tasks[0].status,
        "running",
        "cancel timeout must not fabricate a terminal state"
    );

    let _ = pool.shutdown().await;
    server.abort();
    let _ = server.await;
}
