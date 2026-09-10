use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
};
use serde::Deserialize;

use tracing::{info, warn};

use crate::config::default_shell;
use crate::pty_session::PtySession;
use crate::session_state::SessionState;

#[cfg(unix)]
mod connection;
#[cfg(not(unix))]
#[path = "ws_handler/windows.rs"]
mod connection;
#[cfg(unix)]
mod input;
#[cfg(unix)]
mod io;
mod protocol;

/// 子进程退出检查的固定节拍，不随输入/输出流量重置。
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

/// WebSocket 连接入口：spawn 后将全部 I/O 与 child 生命周期交给平台 owner。
async fn handle_socket(mut socket: WebSocket, q: WsQuery, state: SessionState) {
    let params = q.to_spawn_params();
    let shell_display = params.shell.clone();

    // spawn PTY
    // PtySession::spawn 接收 `&[&str]`，而 SpawnParams.args 是 `Vec<String>`，
    // 需在此处做最小桥接（不修改 spec 规定的公共 API）。
    let args_ref: Vec<&str> = params.args.iter().map(String::as_str).collect();
    let cwd = state.cwd.as_deref();
    let (session, reader) =
        match PtySession::spawn(&params.shell, &args_ref, params.cols, params.rows, cwd) {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("\r\n[failed to spawn {shell_display}: {e}]\r\n");
                warn!("PTY spawn 失败 shell={shell_display} err={e}");
                let _ = socket.send(Message::Text(msg.into())).await;
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        };
    info!(
        "PTY 连接建立 shell={shell_display} cols={} rows={}",
        params.cols, params.rows
    );

    let initial_cmd = if state.try_mark_done() {
        state.initial_cmd.clone()
    } else {
        None
    };
    connection::run(socket, session, reader, initial_cmd).await;
    info!("PTY 连接结束 shell={shell_display}");
}
