//! ConPTY adapter: retains the existing blocking reader and close behavior.
//! Dropping only slave does not close portable-pty's shared Inner; abort cannot
//! interrupt an active read. Native reader cancellation/join still needs Windows validation.
use super::{
    protocol::{exit_message, try_handle_resize, OutputDecoder},
    CHILD_EXIT_POLL_INTERVAL,
};
use crate::pty_session::PtySession;
use axum::extract::ws::{Message, WebSocket};
use std::io::Read;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub(super) async fn run(
    mut socket: WebSocket,
    mut session: PtySession,
    reader: Box<dyn Read + Send>,
    initial_cmd: Option<String>,
) {
    if let Some(cmd) = initial_cmd {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Err(error) = session.write(format!("{cmd}\n").as_bytes()) {
            warn!("初始命令注入失败: {error}");
        }
    }
    // mpsc channel: read_task → pump_task。None 哨兵表示 PTY EOF
    let (tx, mut rx) = mpsc::channel::<Option<Vec<u8>>>(16);

    // Windows ConPTY 启动时 conhost 会向宿主发送 DSR 光标位置查询
    // ESC[6n，宿主不回复 ESC[row;colR 则 conhost 挂起，不输出任何子进程
    // 内容（也不正常收尾）。浏览器端 xterm.js 会自动回复，但其他客户端
    // （SDK、测试、headless 场景）不会——此处由服务端自行响应，不依赖
    // 客户端。writer 经 clone_writer 共享给读取线程。
    let dsr_writer = session.clone_writer();

    // read_task：spawn_blocking 阻塞读 PTY。reader 直接 move 进闭包，无需 Arc<Mutex>
    // 跨读边界 UTF-8 残字节缓冲：多字节字符（中文/CJK、emoji、box-drawing）
    // 可能被 4096 字节缓冲区边界截断，from_utf8_lossy 会产生 �。此处将
    // 不完整尾部字节保存到 leftover，下次 read 时前拼。
    let read_task = tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buffer = [0u8; 4096];
        let mut decoder = OutputDecoder::default();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    let tail = decoder.finish();
                    if !tail.is_empty() {
                        let _ = tx.blocking_send(Some(tail.into_bytes()));
                    }
                    let _ = tx.blocking_send(None);
                    break;
                }
                Ok(count) => {
                    let (text, reply) = decoder.push(&buffer[..count]);
                    if reply {
                        if let Ok(mut writer) = dsr_writer.lock() {
                            let _ = writer.write_all(b"\x1b[1;1R");
                        }
                    }
                    if !text.is_empty() && tx.blocking_send(Some(text.into_bytes())).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut child_poll = tokio::time::interval(CHILD_EXIT_POLL_INTERVAL);
    child_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // pump_task：select! { ws.recv() | rx.recv() | child 退出轮询 }
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if try_handle_resize(text.as_str(), &mut session) {
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
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Some(None) => {
                        // reader EOF：Unix 上几乎等同 child 已退出。
                        // Windows ConPTY 上 child 退出后 reader 不一定返回 EOF
                        // （pty handle 与 IO handle 生命周期不绑定），所以这条
                        // 路径在 Windows 上几乎不会触发，主要靠下面的 polling。
                        session.close_slave();
                        send_exit_message(&mut socket, &mut session).await;
                        break;
                    }
                    None => break, // read_task 退出
                }
            }
            _ = child_poll.tick() => {
                // Windows ConPTY 上 child 退出后 reader.read 永久阻塞不发 EOF，
                // 必须主动轮询 try_wait。Unix 上作为兜底（reader EOF 通常先到）。
                if session.try_wait_exit().ok().flatten().is_some() {
                    session.close_slave();
                    send_exit_message(&mut socket, &mut session).await;
                    break;
                }
            }
        }
    }

    read_task.abort();
    drop(session);
    let _ = socket.send(Message::Close(None)).await;
}

async fn send_exit_message(socket: &mut WebSocket, session: &mut PtySession) {
    let code = session.try_wait_exit().ok().flatten();
    let _ = socket.send(Message::Text(exit_message(code).into())).await;
}
