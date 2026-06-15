//! 端到端集成测试：起真实 axum server，用 tokio-tungstenite client 连接验证协议。

use std::time::Duration;

use axum::{routing::get, Router};
use futures::StreamExt;
use tokio_tungstenite::tungstenite::Message;

use peri_web_pty::http_routes;
use peri_web_pty::session_state::SessionState;
use peri_web_pty::ws_handler;

fn build_app() -> Router {
    Router::new()
        .route("/", get(http_routes::index))
        .route("/ws", get(ws_handler::ws_handler))
        .with_state(SessionState::new(None, None))
}

async fn spawn_server() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = build_app();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    port
}

/// 跨平台获取测试 shell + 退出命令。
///
/// 注意：args 中不能含空格——ws_handler 使用 split_whitespace() 解析，
/// 且 URL 中字面空格是 InvalidUriChar。`exit`（无参）在 bash/cmd 下都合法。
fn exit_shell() -> (&'static str, Vec<&'static str>) {
    if cfg!(target_os = "windows") {
        ("cmd.exe", vec!["/c", "exit"])
    } else {
        ("bash", vec!["-c", "exit"])
    }
}

#[tokio::test]
async fn test_ws_connection_receives_exit_message_on_child_exit() {
    let port = spawn_server().await;
    let (shell, args) = exit_shell();
    let url = format!(
        "ws://127.0.0.1:{port}/ws?shell={shell}&args={}",
        args.join("+")
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // 收消息直到看到 [process exited ...]
    let mut saw_exit = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) if t.contains("[process exited") => {
                saw_exit = true;
                break;
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    assert!(saw_exit, "应收到 [process exited ...]");
}

#[tokio::test]
async fn test_ws_connection_spawn_failure_sends_error_and_closes() {
    let port = spawn_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws?shell=/nonexistent/pty-test-shell");

    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let mut saw_error = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) if t.contains("[failed to spawn") => {
                saw_error = true;
                break;
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    assert!(saw_error, "应收到 [failed to spawn ...]");
}
