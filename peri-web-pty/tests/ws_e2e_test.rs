//! 端到端集成测试：起真实 axum server，用 tokio-tungstenite client 连接验证协议。

use std::time::Duration;

use axum::{routing::get, Router};
use futures::{SinkExt, StreamExt};
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
/// Windows 上用 cmd.exe（`cmd /C exit` 立即退出）而非 powershell.exe：
/// PowerShell 5.1 在 ConPTY 下的启动/退出路径更复杂，cmd 是最稳的验证载体。
fn exit_shell() -> (&'static str, Vec<&'static str>) {
    if cfg!(target_os = "windows") {
        ("cmd.exe", vec!["/C", "exit"])
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
    // 同时模拟 xterm.js 响应 ConPTY 的 DSR 查询（ESC[6n → ESC[1;1R），
    // 服务端已自行响应，此处双保险。
    let mut saw_exit = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) if t.contains("[process exited") => {
                saw_exit = true;
                break;
            }
            Ok(Some(Ok(Message::Text(t)))) if t.contains("\u{1b}[6n") => {
                let _ = ws.send(Message::Text("\u{1b}[1;1R".into())).await;
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
