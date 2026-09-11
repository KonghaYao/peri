//! Unix 单连接 owner：非阻塞 PTY I/O、固定 child 节拍与显式 kill/wait。
use std::collections::VecDeque;
use std::io::{self, Read};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use tokio::time::{Instant, MissedTickBehavior};

use super::input::InputQueue;
use super::io::PtyIo;
use super::protocol::{exit_message, try_handle_resize, OutputDecoder};
use super::CHILD_EXIT_POLL_INTERVAL;
use crate::pty_session::PtySession;

const QUEUE_CAPACITY: usize = 16;

struct Connection {
    session: Option<PtySession>,
    io: Option<PtyIo>,
    cleanup: Option<tokio::task::JoinHandle<io::Result<()>>>,
}

impl Connection {
    async fn new(session: PtySession, reader: Box<dyn Read + Send>) -> io::Result<Self> {
        let io = session.master_fd().and_then(PtyIo::new);
        // Unix owns readiness directly, so no reader thread or cloned DSR writer survives.
        drop(reader);
        match io {
            Ok(io) => Ok(Self {
                session: Some(session),
                io: Some(io),
                cleanup: None,
            }),
            Err(error) => {
                let _ = tokio::task::spawn_blocking(move || session.finish()).await;
                Err(error)
            }
        }
    }

    fn session(&mut self) -> &mut PtySession {
        self.session
            .as_mut()
            .expect("session exists until shutdown")
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        self.io.take();
        if let Some(session) = self.session.take() {
            self.cleanup = Some(tokio::task::spawn_blocking(move || session.finish()));
        }
        if let Some(cleanup) = self.cleanup.as_mut() {
            // Keep ownership across await: a cancelled shutdown can resume the same join.
            let result = cleanup.await;
            self.cleanup.take();
            result.map_err(|error| io::Error::other(error.to_string()))??;
        }
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.io.take();
        // An existing cleanup task is not aborted: dropping the whole owner detaches
        // that task so it can still reap the child. Only completed shutdown proves join.
        if let Some(session) = self.session.take() {
            // Normal exits await shutdown. An externally dropped connection still delegates
            // portable-pty's blocking kill/wait off the runtime worker.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || session.finish());
            } else {
                let _ = session.finish();
            }
        }
    }
}

pub(super) async fn run(
    socket: WebSocket,
    session: PtySession,
    reader: Box<dyn Read + Send>,
    initial_cmd: Option<String>,
) {
    let mut owner = match Connection::new(session, reader).await {
        Ok(owner) => owner,
        Err(error) => {
            tracing::warn!("PTY I/O 初始化失败: {error}");
            return;
        }
    };
    let (mut sink, mut source) = socket.split();
    let mut output = VecDeque::<String>::new();
    let mut input = InputQueue::default();
    let mut needs_flush = false;
    let mut buffer = [0u8; 4096];
    let mut decoder = OutputDecoder::default();
    let mut eof = false;
    let mut exit_code = None;
    let mut exit_queued = false;
    let mut child_poll = tokio::time::interval(CHILD_EXIT_POLL_INTERVAL);
    child_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut initial_cmd = initial_cmd;
    let inject_at = Instant::now() + Duration::from_millis(200);

    loop {
        if eof && exit_code.is_some() && !exit_queued {
            output.push_back(exit_message(exit_code));
            exit_queued = true;
        }
        if exit_queued && output.is_empty() && !needs_flush {
            break;
        }
        let pty = owner.io.as_ref().expect("I/O exists until shutdown");
        tokio::select! {
            message = source.next() => {
                let text = match message {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => { tracing::debug!("WebSocket 接收结束: {error}"); break; }
                };
                if !try_handle_resize(&text, owner.session()) && !input.enqueue(text.into_bytes()) {
                    break;
                }
            }
            read = async {
                if exit_code.is_some() { pty.try_read(&mut buffer) }
                else { pty.read(&mut buffer).await }
            }, if !eof && output.len() < QUEUE_CAPACITY => {
                match read {
                    Ok(0) => {
                        eof = true;
                        let tail = decoder.finish();
                        if !tail.is_empty() { output.push_back(tail); }
                    }
                    Ok(count) => {
                        let (text, reply) = decoder.push(&buffer[..count]);
                        if reply && !input.enqueue(b"\x1b[1;1R".to_vec()) { break; }
                        if !text.is_empty() { output.push_back(text); }
                    }
                    // Once the child was reaped, its writes are already in the PTY. Drain
                    // all currently readable bytes; another slave holder need not send EOF.
                    Err(error) if exit_code.is_some() && error.kind() == io::ErrorKind::WouldBlock => {
                        eof = true;
                        let tail = decoder.finish();
                        if !tail.is_empty() { output.push_back(tail); }
                    }
                    Err(error) => { tracing::debug!("PTY read 结束: {error}"); break; }
                }
            }
            result = pty.write(input.pending()), if !input.pending().is_empty() => {
                match result {
                    Ok(0) => input.close(),
                    Ok(count) => input.advance(count),
                    Err(error) => {
                        tracing::debug!("PTY write 结束: {error}");
                        input.close();
                    }
                }
            }
            result = async {
                if needs_flush { sink.flush().await.map(|()| true) }
                else {
                    sink.feed(Message::Text(output.front().cloned().unwrap_or_default().into())).await.map(|()| false)
                }
            }, if needs_flush || !output.is_empty() => {
                match result {
                    Ok(true) => needs_flush = false,
                    Ok(false) => { output.pop_front(); needs_flush = true; }
                    Err(_) => break,
                }
            }
            _ = child_poll.tick(), if exit_code.is_none() => {
                match owner.session().try_wait_exit() {
                    Ok(code) => {
                        exit_code = code;
                        if exit_code.is_some() {
                            input.close();
                            initial_cmd = None;
                        }
                    }
                    Err(error) => { tracing::warn!("PTY child poll 失败: {error}"); break; }
                }
            }
            _ = tokio::time::sleep_until(inject_at), if initial_cmd.is_some() && input.is_open() => {
                if !input.enqueue(format!("{}\n", initial_cmd.take().expect("pending initial command")).into_bytes()) { break; }
            }
        }
    }
    if let Err(error) = owner.shutdown().await {
        tracing::warn!("PTY cleanup 失败: {error}");
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), sink.send(Message::Close(None))).await;
}

#[cfg(test)]
#[path = "connection_test.rs"]
mod tests;
