use std::io::Read;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::default_shell;
use crate::pty_session::PtySession;
use crate::session_state::SessionState;

/// 子进程退出轮询间隔。
///
/// Windows ConPTY 上 child 退出后 reader.read 永久阻塞不发 EOF（pty handle
/// 与 IO handle 生命周期不绑定），必须主动 try_wait。100ms 足够低延迟，
/// 同时 CPU 开销可忽略（try_wait 是 syscall 但很轻）。
const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// WebSocket 查询参数。
#[derive(Debug, Deserialize)]
pub struct WsQuery {
    pub shell: Option<String>,
    pub args: Option<String>,
    pub cols: Option<String>,
    pub rows: Option<String>,
}

/// 从 WsQuery 解析出的 spawn 参数。
pub struct SpawnParams {
    pub shell: String,
    pub args: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

impl WsQuery {
    /// 把字符串查询参数转为强类型 spawn 参数。
    pub fn to_spawn_params(&self) -> SpawnParams {
        let args = self
            .args
            .as_deref()
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();
        let cols = self
            .cols
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(80);
        let rows = self
            .rows
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(24);
        SpawnParams {
            shell: self
                .shell
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(default_shell),
            args,
            cols,
            rows,
        }
    }
}

/// GET /ws 的 axum handler：升级 WebSocket。
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WsQuery>,
    State(state): State<SessionState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, q, state))
}

/// WebSocket 连接生命周期：spawn PTY + 双向 pump。
async fn handle_socket(mut socket: WebSocket, q: WsQuery, state: SessionState) {
    let params = q.to_spawn_params();
    let shell_display = params.shell.clone();

    // spawn PTY
    // PtySession::spawn 接收 `&[&str]`，而 SpawnParams.args 是 `Vec<String>`，
    // 需在此处做最小桥接（不修改 spec 规定的公共 API）。
    let args_ref: Vec<&str> = params.args.iter().map(String::as_str).collect();
    let cwd = state.cwd.as_deref();
    let (mut session, reader) =
        match PtySession::spawn(&params.shell, &args_ref, params.cols, params.rows, cwd) {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("\r\n[failed to spawn {shell_display}: {e}]\r\n");
                warn!("PTY spawn 失败 shell={shell_display} err={e}");
                let _ = socket.send(Message::Text(msg)).await;
                let _ = socket.close().await;
                return;
            }
        };
    info!(
        "PTY 连接建立 shell={shell_display} cols={} rows={}",
        params.cols, params.rows
    );

    // 第一个 shell 自动注入启动命令
    if state.try_mark_done() {
        if let Some(cmd) = state.initial_cmd.as_deref() {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if let Err(e) = session.write(format!("{cmd}\n").as_bytes()) {
                warn!("初始命令注入失败: {e}");
            }
        }
    }

    // mpsc channel: read_task → pump_task。None 哨兵表示 PTY EOF
    let (tx, mut rx) = mpsc::channel::<Option<Vec<u8>>>(16);

    // read_task：spawn_blocking 阻塞读 PTY。reader 直接 move 进闭包，无需 Arc<Mutex>
    let read_task = tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.blocking_send(None);
                    break;
                }
                Ok(n) => {
                    if tx.blocking_send(Some(buf[..n].to_vec())).is_err() {
                        break; // pump_task 已退出
                    }
                }
            }
        }
    });

    // pump_task：select! { ws.recv() | rx.recv() | child 退出轮询 }
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if try_handle_resize(&text, &mut session) {
                            continue;
                        }
                        if let Err(e) = session.write(text.as_bytes()) {
                            debug!("PTY write 失败（client 输入）: {e}");
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        // 与 Bun 原版一致：binary frame 解码为 UTF-8 后等价于 text frame
                        // （浏览器 xterm.js 通常用 text，但 SDK 可能用 binary）
                        let text = String::from_utf8_lossy(&bytes);
                        if try_handle_resize(&text, &mut session) {
                            continue;
                        }
                        if let Err(e) = session.write(text.as_bytes()) {
                            debug!("PTY write 失败（client 输入 binary）: {e}");
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        debug!("WebSocket 关闭");
                        break;
                    }
                    Some(Ok(_)) => {
                        // Ping/Pong 由 axum 自动处理
                    }
                    Some(Err(e)) => {
                        warn!("WebSocket 接收错误: {e}");
                        break;
                    }
                }
            }
            bytes = rx.recv() => {
                match bytes {
                    Some(Some(data)) => {
                        let text = String::from_utf8_lossy(&data).into_owned();
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Some(None) => {
                        // reader EOF：Unix 上几乎等同 child 已退出。
                        // Windows ConPTY 上 child 退出后 reader 不一定返回 EOF
                        // （pty handle 与 IO handle 生命周期不绑定），所以这条
                        // 路径在 Windows 上几乎不会触发，主要靠下面的 polling。
                        send_exit_message(&mut socket, &mut session).await;
                        break;
                    }
                    None => break, // read_task 退出
                }
            }
            _ = tokio::time::sleep(CHILD_EXIT_POLL_INTERVAL) => {
                // Windows ConPTY 上 child 退出后 reader.read 永久阻塞不发 EOF，
                // 必须主动轮询 try_wait。Unix 上作为兜底（reader EOF 通常先到）。
                if session.try_wait_exit().ok().flatten().is_some() {
                    send_exit_message(&mut socket, &mut session).await;
                    break;
                }
            }
        }
    }

    read_task.abort();
    drop(session);
    let _ = socket.close().await;
    info!("PTY 连接结束 shell={shell_display}");
}

/// 尝试把文本消息当作 resize 命令处理。成功处理返回 true。
fn try_handle_resize(text: &str, session: &mut PtySession) -> bool {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    if parsed.get("type").and_then(|v| v.as_str()) != Some("resize") {
        return false;
    }
    let (Some(cols), Some(rows)) = (
        parsed.get("cols").and_then(|v| v.as_u64()),
        parsed.get("rows").and_then(|v| v.as_u64()),
    ) else {
        return false;
    };
    match session.resize(cols as u16, rows as u16) {
        Ok(()) => true,
        Err(e) => {
            warn!("PTY resize 失败: {e}");
            true // resize 失败也消耗掉这条消息，不当作 stdin
        }
    }
}

/// 发送 `[process exited with code N]` 给 client。退出码未知时显示 "unknown"。
async fn send_exit_message(socket: &mut WebSocket, session: &mut PtySession) {
    let code = session.try_wait_exit().ok().flatten();
    let display = code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let msg = format!("\r\n[process exited with code {display}]\r\n");
    let _ = socket.send(Message::Text(msg)).await;
}
