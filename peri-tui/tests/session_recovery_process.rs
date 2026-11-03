//! 异常退出与再进入的**真实进程**边界（设计 §9、§10 阶段 5）。
//!
//! 两个 `peri acp` 子进程先后打开同一个临时库：
//!
//! 1. 进程 A 收到用户 prompt、在模型请求挂起（等于 turn 中途）时被 SIGKILL；
//! 2. 进程 B 对同一库 `session/load`，必须从中断处恢复：只调用一次模型，且请求里带着
//!    恢复提醒正文；同一进程内再次 `session/load` 不得产生第二次 turn。
//!
//! 隔离：`HOME`、`TMPDIR` 与 `--db-path` 全部指向用例自己的临时目录，模型端点由本用例
//! 的 TCP 服务器充当，不接触真实库、网络或真实模型。

#![cfg(unix)]

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

const RECOVERY_BODY: &str = "会话从中断后的已提交历史继续";
const TURN_TIMEOUT: Duration = Duration::from_secs(60);
const QUIET_WINDOW: Duration = Duration::from_millis(1500);

/// 读一条完整的 HTTP 请求（只在测试内使用，请求体是 ACP 侧发出的模型调用）。
async fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let (body_start, body_len) = loop {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "模型请求不应提前结束");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            assert!(
                headers.starts_with("POST /v1/messages "),
                "模型端点只接受 anthropic 协议：{headers}"
            );
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .unwrap()
                .1
                .trim()
                .parse::<usize>()
                .unwrap();
            break (end + 4, length);
        }
    };
    while bytes.len() < body_start + body_len {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "模型请求 body 不应被截断");
        bytes.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&bytes[body_start..body_start + body_len]).unwrap()
}

/// anthropic 流式响应：一段文本后正常收尾。
async fn respond_text(socket: &mut TcpStream, text: &str) {
    let events = [
        (
            "message_start",
            json!({"message": {"id":"fixture-response", "type":"message", "role":"assistant", "model":"fixture-model", "content":[], "usage":{"input_tokens":1,"output_tokens":0}}}),
        ),
        (
            "content_block_start",
            json!({"index":0, "content_block":{"type":"text","text":""}}),
        ),
        (
            "content_block_delta",
            json!({"index":0, "delta":{"type":"text_delta","text":text}}),
        ),
        ("content_block_stop", json!({"index":0})),
        (
            "message_delta",
            json!({"delta":{"stop_reason":"end_turn"}, "usage":{"input_tokens":100,"output_tokens":7}}),
        ),
        ("message_stop", json!({})),
    ];
    let body = events
        .into_iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect::<String>();
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    socket.shutdown().await.unwrap();
}

/// 记录型模型端点：每次请求体都交给测试线程；第 `hold_first` 次请求故意不回应，
/// 用来让第一个进程停在 turn 中途。
async fn provider(
    listener: TcpListener,
    arrive: mpsc::UnboundedSender<Value>,
    hold_first: Arc<AtomicUsize>,
) {
    loop {
        let (mut socket, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => return,
        };
        let request = read_request(&mut socket).await;
        let index = hold_first.fetch_add(1, Ordering::SeqCst);
        if arrive.send(request.clone()).is_err() {
            return;
        }
        if index == 0 {
            // 进程 A 的 turn：保持连接不回应，直到该进程被 SIGKILL 断开。
            let mut sink = [0; 64];
            let _ = socket.read(&mut sink).await;
            continue;
        }
        respond_text(&mut socket, "recovered-from-crash").await;
    }
}

// ── ACP 子进程 ───────────────────────────────────────────────────────────────

struct AcpProcess {
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stderr_log: std::path::PathBuf,
    next_id: i64,
}

impl AcpProcess {
    fn spawn(root: &Path, db_path: &Path, stderr_log: &Path) -> Self {
        let stderr = std::fs::File::create(stderr_log).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_peri"))
            .env_clear()
            .env("HOME", root)
            .env("TMPDIR", root)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(root)
            .arg("--db-path")
            .arg(db_path)
            .arg("acp")
            .arg("--cwd")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .expect("peri acp 必须能启动");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr_log: stderr_log.to_path_buf(),
            next_id: 1,
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        self.stdin.flush().await.unwrap();
        id
    }

    /// 读到指定 id 的响应；途中的通知按原样返回。
    async fn response(&mut self, id: i64) -> Result<Value, String> {
        loop {
            let line = tokio::time::timeout(TURN_TIMEOUT, self.lines.next_line())
                .await
                .expect("ACP 响应超时")
                .unwrap()
                .expect("ACP 子进程提前退出");
            let value: Value = serde_json::from_str(&line).expect("ACP 输出必须是 JSON");
            if value.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            return match value.get("error") {
                Some(error) => Err(error.to_string()),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
        }
    }

    async fn initialize(&mut self) {
        self.initialize_with_meta(json!({})).await;
    }

    async fn initialize_with_meta(&mut self, meta: Value) {
        let id = self
            .request(
                "initialize",
                json!({"protocolVersion": 1, "clientCapabilities": {"_meta": meta}}),
            )
            .await;
        self.response(id).await.expect("initialize 必须成功");
    }

    async fn kill(mut self) -> String {
        self.child.start_kill().expect("SIGKILL 必须发出");
        let output = self
            .child
            .wait_with_output()
            .await
            .expect("子进程必须可回收");
        assert!(!output.status.success(), "崩溃进程不应正常退出");
        stderr_tail(&self.stderr_log)
    }
}

/// 子进程 stderr 的末段：失败时把它带进 panic，避免只剩超时。
fn stderr_tail(path: &Path) -> String {
    let log = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<&str> = log.lines().rev().take(20).collect();
    lines.reverse();
    lines.join("\n")
}

/// 等模型端点收到下一次请求。
async fn wait_for_request(
    arrive: &mut mpsc::UnboundedReceiver<Value>,
    stderr: impl Fn() -> String,
) -> Value {
    match tokio::time::timeout(TURN_TIMEOUT, arrive.recv()).await {
        Ok(Some(request)) => request,
        Ok(None) => panic!("模型端点提前关闭"),
        Err(_) => panic!("模型请求未到达\n{}", stderr()),
    }
}

fn write_settings(root: &Path, address: std::net::SocketAddr) {
    std::fs::create_dir_all(root.join(".peri")).unwrap();
    std::fs::write(
        root.join(".peri/settings.json"),
        json!({"config":{"providers":[{"id":"fixture", "type":"anthropic", "apiKey":"test-only", "baseUrl":format!("http://{address}"), "models":{"opus":"fixture-model"}}]}})
            .to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn crash_mid_turn_then_load_in_a_new_process_recovers_exactly_once() {
    // 用 `keep()` 保留现场：失败时子进程日志仍在，成功后再显式清理。
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.keep().canonicalize().unwrap();
    let db_path = root.join("threads.db");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    write_settings(&root, listener.local_addr().unwrap());

    let (arrive_tx, mut arrive_rx) = mpsc::unbounded_channel();
    let held = Arc::new(AtomicUsize::new(0));
    let served = tokio::spawn(provider(listener, arrive_tx, Arc::clone(&held)));

    // ── 进程 A：开始一个 turn 后被 SIGKILL ──
    let crashed_log = root.join("crashed.log");
    let mut crashed = AcpProcess::spawn(&root, &db_path, &crashed_log);
    crashed.initialize().await;
    let new_id = crashed
        .request("session/new", json!({"cwd": root.to_str().unwrap()}))
        .await;
    let created = crashed
        .response(new_id)
        .await
        .expect("session/new 必须成功");
    let session_id = created["sessionId"].as_str().unwrap().to_owned();
    let prompt_id = crashed
        .request(
            "session/prompt",
            json!({"sessionId": session_id, "prompt":[{"type":"text","text":"crash me"}]}),
        )
        .await;
    // 模型请求到达 = 工作意图已提交、turn 已开始；prompt 提前返回说明它失败了。
    let first = tokio::select! {
        request = wait_for_request(&mut arrive_rx, || stderr_tail(&crashed_log)) => request,
        response = crashed.response(prompt_id) => panic!("session/prompt 提前返回：{response:?}"),
    };
    assert!(
        first.to_string().contains("crash me"),
        "第一个进程的模型请求应携带用户输入：{first}"
    );
    crashed.kill().await;

    // ── 进程 B：对同一库 session/load，恢复必须发生且只发生一次 ──
    let resumed_log = root.join("resumed.log");
    let mut resumed = AcpProcess::spawn(&root, &db_path, &resumed_log);
    resumed.initialize().await;
    let load_id = resumed
        .request(
            "session/load",
            json!({"sessionId": session_id, "cwd": root.to_str().unwrap()}),
        )
        .await;
    resumed
        .response(load_id)
        .await
        .expect("session/load 必须成功");

    let recovered = wait_for_request(&mut arrive_rx, || stderr_tail(&resumed_log)).await;
    assert!(
        recovered.to_string().contains(RECOVERY_BODY),
        "恢复 turn 必须携带恢复提醒正文：{recovered}"
    );
    assert!(
        recovered.to_string().contains("crash me"),
        "恢复 turn 必须从已提交历史继续：{recovered}"
    );

    // 同一进程再次进入同一根：入口幂等，不得产生第三个 turn。
    let reenter_id = resumed
        .request(
            "session/load",
            json!({"sessionId": session_id, "cwd": root.to_str().unwrap()}),
        )
        .await;
    let _ = resumed.response(reenter_id).await;
    let extra = tokio::time::timeout(QUIET_WINDOW, arrive_rx.recv()).await;
    assert!(
        extra.is_err(),
        "重复进入不得再跑一次恢复 turn：{:?}",
        extra.ok().flatten()
    );
    resumed.kill().await;

    served.abort();
    let _ = served.await;
    assert_eq!(
        held.load(Ordering::SeqCst),
        2,
        "整个过程只允许两次模型请求（崩溃那次 + 恢复那次）"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 崩溃前已入队、尚未发送的用户输入不得让恢复 continuation 失效：新进程 load 后
/// 仍必须真实跑 turn，并按既有 MQ 语义消费提醒与待发输入。
#[tokio::test]
async fn pending_user_input_does_not_supersede_the_recovery_turn() {
    const PENDING: &str = "pending input survives the crash";
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.keep().canonicalize().unwrap();
    let db_path = root.join("threads.db");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    write_settings(&root, listener.local_addr().unwrap());

    let (arrive_tx, mut arrive_rx) = mpsc::unbounded_channel();
    let held = Arc::new(AtomicUsize::new(0));
    let served = tokio::spawn(provider(listener, arrive_tx, Arc::clone(&held)));

    // ── 进程 A：turn 在途时入队一条用户待发输入，然后 SIGKILL ──
    let crashed_log = root.join("crashed-pending.log");
    let mut crashed = AcpProcess::spawn(&root, &db_path, &crashed_log);
    crashed
        .initialize_with_meta(json!({"peri.userInputQueue": true}))
        .await;
    let new_id = crashed
        .request("session/new", json!({"cwd": root.to_str().unwrap()}))
        .await;
    let created = crashed
        .response(new_id)
        .await
        .expect("session/new 必须成功");
    let session_id = created["sessionId"].as_str().unwrap().to_owned();
    let snapshot_id = crashed
        .request("session/input/snapshot", json!({"sessionId": session_id}))
        .await;
    let snapshot = crashed
        .response(snapshot_id)
        .await
        .expect("用户输入队列快照必须成功");
    let generation = snapshot["generation"].as_str().unwrap().to_owned();
    let prompt_id = crashed
        .request(
            "session/prompt",
            json!({"sessionId": session_id, "prompt":[{"type":"text","text":"crash me"}]}),
        )
        .await;
    let first = tokio::select! {
        request = wait_for_request(&mut arrive_rx, || stderr_tail(&crashed_log)) => request,
        response = crashed.response(prompt_id) => panic!("session/prompt 提前返回：{response:?}"),
    };
    assert!(first.to_string().contains("crash me"));
    let enqueue_id = crashed
        .request(
            "session/input/enqueue",
            json!({
                "sessionId": session_id,
                "generation": generation,
                "commandId": "pending-command",
                "inputId": "00000000-0000-4000-8000-000000000001",
                "content": PENDING,
                "originalDraft": PENDING,
            }),
        )
        .await;
    crashed
        .response(enqueue_id)
        .await
        .expect("用户待发输入必须被接纳");
    crashed.kill().await;

    // ── 进程 B：load 后 recovery 必须真的推进 turn ──
    let resumed_log = root.join("resumed-pending.log");
    let mut resumed = AcpProcess::spawn(&root, &db_path, &resumed_log);
    resumed
        .initialize_with_meta(json!({
            "peri.userInputQueue": true,
            "peri.replay": true,
            "peri.agentEvent": true,
            "peri.agentEventDone": true,
            "peri.historyPagination": true,
            "peri.historyWindowV1": true,
            "peri.sessionWorkspaceV1": true,
        }))
        .await;
    let load_id = resumed
        .request(
            "session/load",
            json!({"sessionId": session_id, "cwd": root.to_str().unwrap()}),
        )
        .await;
    resumed
        .response(load_id)
        .await
        .expect("session/load 必须成功");

    let recovered = wait_for_request(&mut arrive_rx, || stderr_tail(&resumed_log)).await;
    assert!(
        recovered.to_string().contains(RECOVERY_BODY),
        "恢复 turn 必须携带提醒正文：{recovered}"
    );
    let after = tokio::time::timeout(TURN_TIMEOUT, arrive_rx.recv()).await;
    let delivered = match after {
        Ok(Some(request)) => request.to_string(),
        Ok(None) => panic!("模型端点提前关闭"),
        Err(_) => panic!("用户待发输入未被继续处理\n{}", stderr_tail(&resumed_log)),
    };
    assert!(
        delivered.contains(PENDING),
        "待发输入必须在恢复后的 turn 中被处理：{delivered}\n{}",
        stderr_tail(&resumed_log)
    );
    resumed.kill().await;

    served.abort();
    let _ = served.await;
    let _ = std::fs::remove_dir_all(&root);
}
