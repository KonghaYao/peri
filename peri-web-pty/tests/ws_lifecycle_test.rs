//! Unix process/PTY fixtures. These do not assert Windows ConPTY cancellation.
#![cfg(unix)]
use axum::{routing::get, Router};
use futures::{SinkExt, StreamExt};
use peri_web_pty::{session_state::SessionState, ws_handler};
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

struct Fixture {
    home: tempfile::TempDir,
    server: tokio::task::JoinHandle<()>,
    port: u16,
}

impl Fixture {
    async fn new(script: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let entry = home.path().join("shell");
        std::fs::write(&entry, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o700)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new()
            .route("/ws", get(ws_handler::ws_handler))
            .with_state(SessionState::new(
                Some(home.path().to_string_lossy().into_owned()),
                None,
            ));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { home, server, port }
    }

    fn url(&self) -> String {
        let shell = self
            .home
            .path()
            .join("shell")
            .to_string_lossy()
            .bytes()
            .map(|byte| format!("%{byte:02X}"))
            .collect::<String>();
        format!("ws://127.0.0.1:{}/ws?shell={shell}", self.port)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The deliberately surviving slave holder is fixture-owned, not an allowed leak.
        if let Ok(pids) = std::fs::read_to_string(self.home.path().join("pids")) {
            for pid in pids
                .split_whitespace()
                .filter_map(|value| value.parse::<i32>().ok())
            {
                if pid > 0 {
                    // SAFETY: pids are written by this fixture immediately after spawning.
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
        self.server.abort();
    }
}

struct ThreadDeadline {
    stop: mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    expired: Arc<AtomicBool>,
}

impl ThreadDeadline {
    fn new(timeout: Duration, cleanup: impl FnOnce() + Send + 'static) -> Self {
        let (stop, receiver) = mpsc::channel();
        let expired = Arc::new(AtomicBool::new(false));
        let flag = expired.clone();
        let worker = std::thread::spawn(move || {
            if matches!(
                receiver.recv_timeout(timeout),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                flag.store(true, Ordering::SeqCst);
                cleanup();
            }
        });
        Self {
            stop,
            worker: Some(worker),
            expired,
        }
    }
}

impl Drop for ThreadDeadline {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        let _ = self.worker.take().unwrap().join();
    }
}

#[test]
fn test_child_exit_during_continuous_input_with_saturated_blocking_pool() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (release_slot, slot) = mpsc::channel();
    let (occupied, occupied_rx) = mpsc::channel();
    let _blocker = runtime.spawn_blocking(move || {
        occupied.send(()).unwrap();
        let _ = slot.recv();
    });
    occupied_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    runtime.block_on(async {
        let fixture = Fixture::new(r#"
printf '%s' "$$" > pids
touch ready
while [ ! -e release ]; do sleep 0.01; done
printf FINAL-TAIL
exit 7
"#).await;
        let (mut socket, _) = tokio_tungstenite::connect_async(fixture.url()).await.unwrap();
        // The legacy blocking reader cannot run, so readiness comes from the real
        // child filesystem signal rather than waiting for PTY output through that reader.
        tokio::time::timeout(Duration::from_secs(3), async {
            while !fixture.home.path().join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        let release = release_slot.clone();
        let watchdog = ThreadDeadline::new(Duration::from_secs(2), move || { let _ = release.send(()); });
        let started = Instant::now();
        std::fs::write(fixture.home.path().join("release"), b"go").unwrap();
        let mut input = tokio::time::interval(Duration::from_millis(5));
        let mut sent = 0;
        let mut pongs = 0;
        let exited = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    _ = input.tick() => {
                        socket.send(Message::Ping(Vec::new().into())).await.unwrap();
                        sent += 1;
                    }
                    message = socket.next() => match message {
                        Some(Ok(Message::Pong(_))) => pongs += 1,
                        Some(Ok(Message::Text(text))) if text.contains("[process exited with code 7]") => break true,
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break false,
                        _ => {}
                    }
                }
            }
        }).await.unwrap_or(false);
        let elapsed = started.elapsed();
        assert!(sent > 0 && pongs > 0, "fixture must exchange actual Ping/Pong frames");
        assert!(exited && !watchdog.expired.load(Ordering::SeqCst) && elapsed < Duration::from_secs(2),
            "exit must be observed without releasing the blocked reader; elapsed={elapsed:?}, sent={sent}, pongs={pongs}, watchdog={}", watchdog.expired.load(Ordering::SeqCst));
    });
    drop(release_slot);
}

#[tokio::test]
async fn test_all_multibyte_tail_output_precedes_exit_message() {
    let fixture = Fixture::new(
        r#"
i=0
while [ "$i" -lt 8192 ]; do printf '终端🙂|'; i=$((i+1)); done
printf 'FINAL-TAIL'
exit 7
"#,
    )
    .await;
    let (mut socket, _) = tokio_tungstenite::connect_async(fixture.url())
        .await
        .unwrap();
    let mut output = String::new();
    let exited = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) if text.contains("[process exited") => {
                    assert_eq!(text, "\r\n[process exited with code 7]\r\n");
                    break true;
                }
                Some(Ok(Message::Text(text))) => output.push_str(&text),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break false,
                _ => {}
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(exited);
    assert_eq!(output, format!("{}FINAL-TAIL", "终端🙂|".repeat(8192)));
}

#[tokio::test]
async fn test_disconnect_reaps_child() {
    let fixture = Fixture::new(
        r#"
printf '%s' "$$" > pids
printf 'READY\n'
while :; do sleep 1; done
"#,
    )
    .await;
    let (mut socket, _) = tokio_tungstenite::connect_async(fixture.url())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(Ok(Message::Text(text))) = socket.next().await {
                if text.contains("READY") {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(fixture.home.path().join("pids"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    socket.send(Message::Close(None)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(message) = socket.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
        }
    })
    .await
    .expect("connection should finish cleanup promptly");
    assert!(pid > 0);
    // WebSocket's automatic Close reply can precede application shutdown. Check the
    // actual child instead of treating that protocol reply as proof of cleanup.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            // SAFETY: signal zero probes the positive pid supplied by this fixture.
            if unsafe { libc::kill(pid, 0) } == -1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("disconnect must reap the shell");
    // SAFETY: same fixture-owned pid; a completed wait must make it disappear.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    // Do not signal a reaped pid from the fixture guard.
    std::fs::remove_file(fixture.home.path().join("pids")).unwrap();
}

#[tokio::test]
async fn test_text_and_binary_frames_share_resize_and_stdin_protocol() {
    let fixture = Fixture::new(
        r#"
stty -echo
printf 'READY\n'
while read command; do
    case "$command" in size) stty size;; quit) exit 0;; esac
done
"#,
    )
    .await;
    let (mut socket, _) = tokio_tungstenite::connect_async(fixture.url())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(Ok(Message::Text(text))) = socket.next().await {
                if text.contains("READY") {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    for (resize, stdin, expected) in [
        (
            Message::Text(r#"{"type":"resize","cols":100,"rows":30}"#.into()),
            Message::Binary(b"size\n".to_vec().into()),
            "30 100",
        ),
        (
            Message::Binary(br#"{"type":"resize","cols":80,"rows":24}"#.to_vec().into()),
            Message::Text("size\n".into()),
            "24 80",
        ),
    ] {
        socket.send(resize).await.unwrap();
        socket.send(stdin).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut output = String::new();
            loop {
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => {
                        output.push_str(&text);
                        if output.contains(expected) {
                            break;
                        }
                    }
                    message => panic!("expected PTY size response, got {message:?}"),
                }
            }
        })
        .await
        .unwrap();
    }
    socket.send(Message::Text("quit\n".into())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(message) = socket.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_disconnect_cancels_write_to_a_full_pty_input_buffer() {
    let fixture = Fixture::new(
        r#"
stty raw -echo
printf '%s' "$$" > pids
printf 'READY'
exec sleep 30
"#,
    )
    .await;
    let (mut socket, _) = tokio_tungstenite::connect_async(fixture.url())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(Ok(Message::Text(text))) = socket.next().await {
                if text.contains("READY") {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(fixture.home.path().join("pids"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(pid > 0);
    let watchdog = ThreadDeadline::new(Duration::from_secs(3), move || {
        // SAFETY: the independent watchdog owns this fixture pid for failure cleanup.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    });
    let started = Instant::now();
    // Raw-mode sleep never consumes stdin; this frame exceeds the kernel's PTY input queue.
    let sent = socket
        .send(Message::Binary(vec![b'x'; 1024 * 1024].into()))
        .await;
    let closed = socket.send(Message::Close(None)).await;
    let reaped = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            // SAFETY: signal zero probes this fixture's process without modifying it.
            if unsafe { libc::kill(pid, 0) } == -1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        sent.is_ok()
            && closed.is_ok()
            && reaped.is_ok()
            && !watchdog.expired.load(Ordering::SeqCst)
            && started.elapsed() < Duration::from_secs(3),
        "full PTY write blocked disconnect/reap: elapsed={:?}, watchdog={}",
        started.elapsed(),
        watchdog.expired.load(Ordering::SeqCst)
    );
    std::fs::remove_file(fixture.home.path().join("pids")).unwrap();
}
