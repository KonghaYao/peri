//! Print-mode async-task settlement scenarios.
//!
//! These tests run the real `peri` binary (ACP host, agent loop, session close,
//! shell executor) against a fixture HTTP provider. Only the model wire is
//! mocked. The bounded wait is shortened through the `test-support` feature;
//! production builds keep the 120s default.
//!
//! Each test records the ACP terminal state (`_meta.pendingTasks`), the print
//! projection, the exit code, whether the shell child is still alive after
//! exit, and whether the completion result reached a later model request.

#![cfg(feature = "test-support")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

const ANSWER: &str = "settlement-answer";
const CHILD_MARKER: &str = "CHILD-SETTLEMENT-MARKER";

#[derive(Clone, Debug)]
enum Script {
    /// Model ends the turn; the shell task outlives the bounded wait.
    Deadline { command: String },
    /// Model ends the turn; the shell task completes inside the window.
    WithinWindow { command: String },
    /// Task completes at (approximately) the deadline.
    Race { command: String },
    /// Active child that ignores TERM, exercising session close.
    Close { command: String },
    /// Foreground sub-agent owns a background shell task.
    Nested {
        parent_command: String,
        child_command: String,
    },
}

struct RunOutcome {
    output: std::process::Output,
    requests: Vec<Value>,
    parent_steps: usize,
    child_steps: usize,
    elapsed: Duration,
    dir: tempfile::TempDir,
}

impl RunOutcome {
    fn stdout_json(&self) -> Option<Value> {
        serde_json::from_slice(&self.output.stdout).ok()
    }

    fn print_response(&self) -> Option<Value> {
        parse_prefixed_json(&self.output.stderr, "PERI_TEST_PRINT_RESPONSE ")
    }

    fn pending_tasks(&self) -> Value {
        self.print_response()
            .and_then(|v| v.get("_meta").cloned())
            .and_then(|m| m.get("peri").cloned())
            .and_then(|p| p.get("pendingTasks").cloned())
            .unwrap_or(json!(null))
    }

    fn stop_reason(&self) -> Value {
        self.print_response()
            .and_then(|v| v.get("stopReason").cloned())
            .unwrap_or(json!(null))
    }

    fn close_diag(&self) -> Option<String> {
        self.output
            .stderr
            .split(|byte| *byte == b'\n')
            .find_map(|line| {
                std::str::from_utf8(line)
                    .ok()
                    .and_then(|line| line.strip_prefix("PERI_TEST_SESSION_CLOSE "))
                    .map(str::to_owned)
            })
    }

    fn exit_code(&self) -> Option<i32> {
        self.output.status.code()
    }

    fn status(&self) -> Value {
        self.stdout_json()
            .and_then(|v| v.get("status").cloned())
            .unwrap_or(json!(null))
    }

    fn db_has_pending_handoff(&self) -> bool {
        let db = self.dir.path().join("threads.db");
        ["", "-wal", "-shm"].iter().any(|suffix| {
            let path = PathBuf::from(format!("{}{suffix}", db.display()));
            let bytes = std::fs::read(path).unwrap_or_default();
            contains(&bytes, b"pending_handoff")
        })
    }

    fn child_alive(&self, pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn parse_prefixed_json(stderr: &[u8], prefix: &str) -> Option<Value> {
    stderr
        .split(|byte| *byte == b'\n')
        .find_map(|line| {
            std::str::from_utf8(line)
                .ok()
                .and_then(|line| line.strip_prefix(prefix))
        })
        .and_then(|json| serde_json::from_str(json).ok())
}

fn is_prediction_request(body: &Value) -> bool {
    body.to_string().contains("<prediction_directive>")
}

/// Child requests do not inherit the parent's `Agent` tool.
fn is_child_request(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            !tools
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some("Agent"))
        })
        .unwrap_or(false)
}

fn sse_frames(events: &[(&str, Value)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn text_response(text: &str) -> String {
    sse_frames(&[
        (
            "message_start",
            json!({"message": {
                "id": "fixture-response", "type": "message", "role": "assistant",
                "model": "fixture-model", "content": [],
                "usage": {"input_tokens": 1, "output_tokens": 0}
            }}),
        ),
        (
            "content_block_start",
            json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        (
            "content_block_delta",
            json!({"index": 0, "delta": {"type": "text_delta", "text": text}}),
        ),
        ("content_block_stop", json!({"index": 0})),
        (
            "message_delta",
            json!({"delta": {"stop_reason": "end_turn"},
                   "usage": {"input_tokens": 10, "output_tokens": 2}}),
        ),
        ("message_stop", json!({})),
    ])
}

fn tool_response(id: &str, name: &str, input: Value) -> String {
    sse_frames(&[
        (
            "message_start",
            json!({"message": {
                "id": "fixture-response", "type": "message", "role": "assistant",
                "model": "fixture-model", "content": [],
                "usage": {"input_tokens": 1, "output_tokens": 0}
            }}),
        ),
        (
            "content_block_start",
            json!({"index": 0, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}),
        ),
        (
            "content_block_delta",
            json!({"index": 0, "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
        ),
        ("content_block_stop", json!({"index": 0})),
        (
            "message_delta",
            json!({"delta": {"stop_reason": "tool_use"},
                   "usage": {"input_tokens": 10, "output_tokens": 2}}),
        ),
        ("message_stop", json!({})),
    ])
}

async fn respond(socket: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    socket
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    socket.shutdown().await.unwrap();
}

fn route(
    script: &Script,
    body: &Value,
    parent_steps: &AtomicUsize,
    child_steps: &AtomicUsize,
) -> String {
    match script {
        Script::Nested { child_command, .. } if is_child_request(body) => {
            let step = child_steps.fetch_add(1, Ordering::SeqCst);
            if step == 0 {
                tool_response(
                    "child-bash-1",
                    "Bash",
                    json!({"command": child_command, "run_in_background": true}),
                )
            } else {
                text_response("child-final-answer")
            }
        }
        Script::Nested { .. } => {
            let step = parent_steps.fetch_add(1, Ordering::SeqCst);
            if step == 0 {
                tool_response(
                    "parent-agent-1",
                    "Agent",
                    json!({
                        "subagent_type": "general-purpose",
                        "prompt": format!("{CHILD_MARKER}: run the delegated settlement probe"),
                        "run_in_background": false
                    }),
                )
            } else if step == 1 {
                text_response("parent-final-answer")
            } else {
                text_response("parent-consumed-completion")
            }
        }
        Script::Deadline { command } | Script::Close { command } => {
            let step = parent_steps.fetch_add(1, Ordering::SeqCst);
            if step == 0 {
                tool_response(
                    "parent-bash-1",
                    "Bash",
                    json!({"command": command, "run_in_background": true}),
                )
            } else {
                text_response(ANSWER)
            }
        }
        Script::WithinWindow { command } | Script::Race { command } => {
            let step = parent_steps.fetch_add(1, Ordering::SeqCst);
            match step {
                0 => tool_response(
                    "parent-bash-1",
                    "Bash",
                    json!({"command": command, "run_in_background": true}),
                ),
                1 => text_response("waiting-for-background-task"),
                _ => text_response("consumed-completion"),
            }
        }
    }
}

async fn run_case(script: Script, window_ms: u64, timeout: Duration) -> RunOutcome {
    let fixture = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let settings = fixture.path().join("settings.json");
    std::fs::write(
        &settings,
        json!({"config": {
            "providers": [{
                "id": "fixture", "type": "anthropic", "apiKey": "test-only",
                "baseUrl": format!("http://{address}"),
                "models": {"opus": "fixture-model"}
            }]
        }})
        .to_string(),
    )
    .unwrap();

    let requests = Arc::new(Mutex::new(Vec::new()));
    let parent_steps = Arc::new(AtomicUsize::new(0));
    let child_steps = Arc::new(AtomicUsize::new(0));
    let provider = tokio::spawn({
        let requests = Arc::clone(&requests);
        let parent_steps = Arc::clone(&parent_steps);
        let child_steps = Arc::clone(&child_steps);
        let script = script.clone();
        async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let (header_end, content_length) = loop {
                    let mut buffer = [0; 4096];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0, "provider request ended before headers");
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&request[..end]).unwrap();
                        assert!(headers.starts_with("POST /v1/messages "));
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
                while request.len() < header_end + content_length {
                    let mut buffer = [0; 4096];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0, "provider request body was truncated");
                    request.extend_from_slice(&buffer[..count]);
                }
                let body: Value = serde_json::from_slice(&request[header_end..]).unwrap();
                if is_prediction_request(&body) {
                    let frames = sse_frames(&[
                        (
                            "message_start",
                            json!({"message": {
                                "id": "fixture-prediction", "type": "message", "role": "assistant",
                                "model": "fixture-model", "content": [],
                                "usage": {"input_tokens": 1, "output_tokens": 0}
                            }}),
                        ),
                        (
                            "content_block_start",
                            json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
                        ),
                        (
                            "content_block_delta",
                            json!({"index": 0, "delta": {"type": "text_delta", "text": ""}}),
                        ),
                        ("content_block_stop", json!({"index": 0})),
                        (
                            "message_delta",
                            json!({"delta": {"stop_reason": "end_turn"},
                                   "usage": {"input_tokens": 1, "output_tokens": 1}}),
                        ),
                        ("message_stop", json!({})),
                    ]);
                    respond(&mut socket, "200 OK", "text/event-stream", &frames).await;
                    continue;
                }
                requests.lock().unwrap().push(body.clone());
                let response = route(&script, &body, &parent_steps, &child_steps);
                respond(&mut socket, "200 OK", "text/event-stream", &response).await;
            }
        }
    });

    let mut command = Command::new(env!("CARGO_BIN_EXE_peri"));
    command
        .env_clear()
        .env("HOME", fixture.path())
        .env("USERPROFILE", fixture.path())
        .env("TMPDIR", fixture.path())
        .env("TEMP", fixture.path())
        .env("TMP", fixture.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("PERI_TEST_HANDOFF_MAX_WAIT_MS", window_ms.to_string())
        .env("PERI_TEST_PRINT_DIAG", "1")
        .env("RUST_LOG", "warn")
        .current_dir(fixture.path())
        .args(["--print", "settlement probe", "--output-format", "json"])
        .arg("--config-file")
        .arg(&settings)
        .arg("--settings")
        .arg(&settings)
        .arg("--db-path")
        .arg(fixture.path().join("threads.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }

    let started = Instant::now();
    let mut child = command.spawn().unwrap();
    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            child.wait(),
            stdout_pipe.read_to_end(&mut stdout),
            stderr_pipe.read_to_end(&mut stderr),
        )
    })
    .await;
    let elapsed = started.elapsed();
    let status = match result {
        Ok(result) => result.unwrap().0,
        Err(_) => {
            provider.abort();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.kill()).await;
            panic!(
                "print did not exit within {timeout:?}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr),
            );
        }
    };
    provider.abort();
    let _ = provider.await;

    RunOutcome {
        output: std::process::Output {
            status,
            stdout,
            stderr,
        },
        requests: requests.lock().unwrap().clone(),
        parent_steps: parent_steps.load(Ordering::SeqCst),
        child_steps: child_steps.load(Ordering::SeqCst),
        elapsed,
        dir: fixture,
    }
}

fn read_trimmed(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn write_probe(
    dir: &Path,
    sleep_secs: &str,
    ignore_term: bool,
) -> (String, PathBuf, PathBuf, PathBuf) {
    let pidfile = dir.join("shell.pid");
    let siglog = dir.join("shell.signal.log");
    let donelog = dir.join("shell.done.log");
    let trap = if ignore_term {
        "trap '' TERM".to_string()
    } else {
        format!("trap 'echo TERM >> {}; exit 143' TERM", siglog.display())
    };
    let command = format!(
        "{trap}; echo $$ > {}; sleep {sleep_secs}; echo DONE >> {}",
        pidfile.display(),
        donelog.display()
    );
    (command, pidfile, siglog, donelog)
}

fn cleanup_pid(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

// ─── Scenario 1: task outlives the bounded wait ─────────────────────────────

#[tokio::test]
async fn scenario1_deadline_with_active_shell_task() {
    let fixture = tempfile::tempdir().unwrap();
    let (command, pidfile, siglog, _done) = write_probe(fixture.path(), "30", false);
    let outcome = run_case(Script::Deadline { command }, 2_000, Duration::from_secs(40)).await;

    let pid: Option<u32> = read_trimmed(&pidfile).parse().ok();
    let alive_after = pid.is_some_and(|pid| outcome.child_alive(pid));

    eprintln!(
        "[scenario1] elapsed={:?} exit={:?} status={} stop={} pending={} parent_steps={} child_steps={} close={:?} handoff_db={} pid={pid:?} alive_after={alive_after} sig={}",
        outcome.elapsed,
        outcome.exit_code(),
        outcome.status(),
        outcome.stop_reason(),
        outcome.pending_tasks(),
        outcome.parent_steps,
        outcome.child_steps,
        outcome.close_diag(),
        outcome.db_has_pending_handoff(),
        read_trimmed(&siglog),
    );

    // Mechanism: the deadline expires with the task unsettled → EndTurn with
    // pendingTasks; print must not project that as success.
    assert_eq!(outcome.pending_tasks(), json!(1));
    assert_eq!(outcome.stop_reason(), json!("end_turn"));
    assert_eq!(outcome.status(), json!("incomplete"));
    assert_ne!(outcome.exit_code(), Some(0));
    assert_eq!(
        outcome.stdout_json().unwrap()["pending_tasks"],
        json!(1),
        "the result JSON must expose the unsettled task count"
    );
    assert_eq!(
        outcome.parent_steps, 2,
        "no completion turn may be invented"
    );
    assert!(
        outcome.db_has_pending_handoff(),
        "the bounded wait must leave a canonical pending_handoff record"
    );

    // The close path must not silently leak the shell child. Record and clean
    // up whatever survives so the test run does not leave processes behind.
    if let Some(pid) = pid {
        if alive_after {
            cleanup_pid(pid);
        }
    }
    assert!(
        !alive_after,
        "session close left the shell child alive (pid {pid:?}); sig={:?}",
        read_trimmed(&siglog)
    );
}

// ─── Scenario 2: completion inside the window is consumed ───────────────────

#[tokio::test]
async fn scenario2_completion_inside_window_reaches_next_request() {
    let fixture = tempfile::tempdir().unwrap();
    let (command, pidfile, siglog, donelog) = write_probe(fixture.path(), "0.8", false);
    let outcome = run_case(
        Script::WithinWindow { command },
        6_000,
        Duration::from_secs(30),
    )
    .await;

    let pid: Option<u32> = read_trimmed(&pidfile).parse().ok();
    let completion_request = outcome.parent_steps >= 3;
    let body_has_completion = outcome
        .requests
        .get(2)
        .map(|body| body.to_string().contains("后台任务结果"))
        .unwrap_or(false);

    eprintln!(
        "[scenario2] elapsed={:?} exit={:?} status={} stop={} pending={} parent_steps={} completion_request={completion_request} body_has_completion={body_has_completion} close={:?} pid={pid:?} alive={} done={} sig={}",
        outcome.elapsed,
        outcome.exit_code(),
        outcome.status(),
        outcome.stop_reason(),
        outcome.pending_tasks(),
        outcome.parent_steps,
        outcome.close_diag(),
        pid.is_some_and(|pid| outcome.child_alive(pid)),
        read_trimmed(&donelog),
        read_trimmed(&siglog),
    );

    // Counterexample: the completion result is delivered to a later model
    // request and the session exits without pending work.
    assert!(
        completion_request,
        "completion must trigger another model request (parent_steps={})",
        outcome.parent_steps
    );
    assert!(
        body_has_completion,
        "the completion notification must be in the third request: {:?}",
        outcome.requests.get(2).map(ToString::to_string)
    );
    assert_eq!(outcome.pending_tasks(), json!(null));
    assert_eq!(outcome.status(), json!("completed"));
    assert_eq!(outcome.exit_code(), Some(0));
    assert_eq!(outcome.stdout_json().unwrap()["pending_tasks"], json!(0));
    assert_eq!(read_trimmed(&donelog), "DONE");
    assert_eq!(
        read_trimmed(&siglog),
        "",
        "a normally completed task must not be TERM'd"
    );
}

// ─── Scenario 3: completion at the deadline (race) ──────────────────────────

#[tokio::test]
async fn scenario3_deadline_race_repeated_runs() {
    // 2.0s window; sweep the task duration across the boundary. Two runs per
    // step so both outcomes are sampled even with process-start jitter.
    let mut rows = Vec::new();
    for (index, sleep_secs) in [
        "1.90", "1.95", "2.00", "2.05", "2.10", "2.15", "2.20", "2.25", "2.30", "2.35", "2.40",
    ]
    .into_iter()
    .enumerate()
    {
        for repeat in 0..2 {
            let fixture = tempfile::tempdir().unwrap();
            let (command, pidfile, siglog, _done) = write_probe(fixture.path(), sleep_secs, false);
            let outcome = run_case(Script::Race { command }, 2_000, Duration::from_secs(25)).await;
            let pid: Option<u32> = read_trimmed(&pidfile).parse().ok();
            let alive_after = pid.is_some_and(|pid| outcome.child_alive(pid));
            let row = json!({
                "sleep_ms": (sleep_secs.parse::<f64>().unwrap() * 1000.0) as u64,
                "run": index * 2 + repeat,
                "parent_steps": outcome.parent_steps,
                "completion_turn": outcome.parent_steps >= 3,
                "pending_tasks": outcome.pending_tasks(),
                "status": outcome.status(),
                "exit": outcome.exit_code(),
                "close": outcome.close_diag(),
                "alive_after": alive_after,
                "sig": read_trimmed(&siglog),
            });
            eprintln!("[scenario3] {row}");
            if let Some(pid) = pid {
                if alive_after {
                    cleanup_pid(pid);
                }
            }
            rows.push(row);
        }
    }

    let completion_turns = rows
        .iter()
        .filter(|row| row["completion_turn"] == json!(true))
        .count();
    let deadline_turns = rows.len() - completion_turns;
    let inconsistent = rows
        .iter()
        .filter(|row| row["completion_turn"] == json!(true) && row["pending_tasks"] == json!(1))
        .count();
    eprintln!(
        "[scenario3-summary] runs={} completion_turns={completion_turns} deadline_turns={deadline_turns} both_consumed_and_pending={inconsistent}",
        rows.len()
    );
    // Every row must agree: an unsettled task is exactly the case print reports
    // as incomplete with a non-zero exit.
    let projection_mismatches = rows
        .iter()
        .filter(|row| {
            let pending = row["pending_tasks"] == json!(1);
            let not_completed = row["status"] == json!("incomplete") && row["exit"] != json!(0);
            pending != not_completed
        })
        .count();
    assert_eq!(
        projection_mismatches, 0,
        "pending==1 must be equivalent to incomplete && exit!=0: {rows:?}"
    );
    assert_eq!(rows.len(), 22, "at least 20 repeated runs are required");
    assert_eq!(
        inconsistent, 0,
        "a consumed completion must not stay pending"
    );
    // Both outcomes must actually be observed near the boundary.
    assert!(
        completion_turns > 0 && deadline_turns > 0,
        "sweep did not straddle the deadline: {completion_turns}/{deadline_turns}"
    );
}

// ─── Scenario 4: close with an active, TERM-ignoring child ──────────────────

#[tokio::test]
async fn scenario4_close_with_active_child() {
    let fixture = tempfile::tempdir().unwrap();
    let (command, pidfile, siglog, _done) = write_probe(fixture.path(), "30", true);
    let outcome = run_case(Script::Close { command }, 1_500, Duration::from_secs(40)).await;
    let pid: Option<u32> = read_trimmed(&pidfile).parse().ok();
    let alive_after = pid.is_some_and(|pid| outcome.child_alive(pid));

    eprintln!(
        "[scenario4] elapsed={:?} exit={:?} status={} stop={} pending={} close={:?} pid={pid:?} alive_after={alive_after} sig={}",
        outcome.elapsed,
        outcome.exit_code(),
        outcome.status(),
        outcome.stop_reason(),
        outcome.pending_tasks(),
        outcome.close_diag(),
        read_trimmed(&siglog),
    );

    // The terminal response is still EndTurn; close may fail or may SIGKILL the
    // child. Record which one happened and require cleanup in either case.
    assert_eq!(outcome.stop_reason(), json!("end_turn"));
    assert_eq!(outcome.pending_tasks(), json!(1));
    assert_eq!(outcome.status(), json!("incomplete"));
    assert_ne!(outcome.exit_code(), Some(0));
    assert_eq!(outcome.stdout_json().unwrap()["pending_tasks"], json!(1));
    if let Some(pid) = pid {
        if alive_after {
            cleanup_pid(pid);
        }
    }
    assert!(
        !alive_after,
        "close returned (diag {:?}) but left the child alive; sig={:?}",
        outcome.close_diag(),
        read_trimmed(&siglog)
    );
}

// ─── Scenario 4b: detached descendant keeps the scope alive → close rejected ─

#[tokio::test]
async fn scenario4b_close_rejected_with_detached_descendant() {
    let fixture = tempfile::tempdir().unwrap();
    let shell_pidfile = fixture.path().join("shell.pid");
    let detached_pidfile = fixture.path().join("detached.pid");
    let siglog = fixture.path().join("shell.signal.log");
    // The detached perl calls setsid(): group TERM/KILL cannot reach it, and it
    // keeps the inherited stdout/stderr pipes open, so the task worker cannot
    // settle. This is the real precondition for `scope.wait()` to time out.
    let command = format!(
        "trap 'echo TERM >> {}; exit 143' TERM; \
         perl -MPOSIX -e 'POSIX::setsid(); sleep 30' & \
         echo $! > {}; echo $$ > {}; sleep 30",
        siglog.display(),
        detached_pidfile.display(),
        shell_pidfile.display()
    );
    let outcome = run_case(Script::Close { command }, 1_500, Duration::from_secs(40)).await;
    let shell_pid: Option<u32> = read_trimmed(&shell_pidfile).parse().ok();
    let detached_pid: Option<u32> = read_trimmed(&detached_pidfile).parse().ok();
    let shell_alive = shell_pid.is_some_and(|pid| outcome.child_alive(pid));
    let detached_alive = detached_pid.is_some_and(|pid| outcome.child_alive(pid));

    eprintln!(
        "[scenario4b] elapsed={:?} exit={:?} status={} stop={} pending={} close={:?} shell_pid={shell_pid:?} shell_alive={shell_alive} detached_pid={detached_pid:?} detached_alive={detached_alive} sig={}",
        outcome.elapsed,
        outcome.exit_code(),
        outcome.status(),
        outcome.stop_reason(),
        outcome.pending_tasks(),
        outcome.close_diag(),
        read_trimmed(&siglog),
    );

    // The turn itself succeeded, but close could not settle the escaped
    // descendant: print reports a cleanup failure and exits non-zero.
    assert_eq!(outcome.stop_reason(), json!("end_turn"));
    assert_eq!(outcome.status(), json!("incomplete"));
    assert_eq!(outcome.pending_tasks(), json!(1));
    let close = outcome.close_diag().unwrap_or_default();
    assert!(
        close.starts_with("error="),
        "close must be rejected when the scope cannot settle in 5s: {close:?}"
    );
    assert_eq!(
        outcome.exit_code(),
        Some(1),
        "a cleanup failure must not be reported as a successful print run"
    );
    assert!(!shell_alive, "the direct child must be killed by close");
    assert!(
        detached_alive,
        "documents the leak: a setsid descendant survives close"
    );

    if let Some(pid) = detached_pid {
        cleanup_pid(pid);
    }
}

// ─── Scenario 5: nested sub-agent owns the background shell ─────────────────

#[tokio::test]
async fn scenario5_nested_child_task_settlement() {
    let fixture = tempfile::tempdir().unwrap();
    let (parent_command, _parent_pid, _parent_sig, _parent_done) =
        write_probe(fixture.path(), "0.1", false);
    let child_pid = fixture.path().join("child.pid");
    let child_sig = fixture.path().join("child.signal.log");
    let child_command = format!(
        "trap 'echo TERM >> {}; exit 143' TERM; echo $$ > {}; sleep 30",
        child_sig.display(),
        child_pid.display()
    );
    let outcome = run_case(
        Script::Nested {
            parent_command,
            child_command,
        },
        2_500,
        Duration::from_secs(45),
    )
    .await;
    let pid: Option<u32> = read_trimmed(&child_pid).parse().ok();
    let alive_after = pid.is_some_and(|pid| outcome.child_alive(pid));

    let parent_completion_request = outcome.parent_steps >= 2;
    let child_completion_in_parent = outcome
        .requests
        .iter()
        .skip(1)
        .any(|body| !is_child_request(body) && body.to_string().contains("pending_tasks"));

    eprintln!(
        "[scenario5] elapsed={:?} exit={:?} status={} stop={} pending={} parent_steps={} child_steps={} parent_completion_request={parent_completion_request} child_completion_in_parent={child_completion_in_parent} close={:?} child_pid={pid:?} alive_after={alive_after} sig={}",
        outcome.elapsed,
        outcome.exit_code(),
        outcome.status(),
        outcome.stop_reason(),
        outcome.pending_tasks(),
        outcome.parent_steps,
        outcome.child_steps,
        outcome.close_diag(),
        read_trimmed(&child_sig),
    );
    for (index, body) in outcome.requests.iter().enumerate() {
        let tool_names: Vec<&str> = body
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|tool| tool.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        let text = body.to_string();
        let mut tool_use_commands = Vec::new();
        let mut tool_use_count = 0usize;
        if let Some(messages) = body.get("messages").and_then(Value::as_array) {
            for message in messages {
                if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                    for block in blocks {
                        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                            tool_use_count += 1;
                            if block.get("name").and_then(Value::as_str) == Some("Bash") {
                                tool_use_commands.push(
                                    block
                                        .pointer("/input/command")
                                        .and_then(Value::as_str)
                                        .unwrap_or("<missing>")
                                        .to_owned(),
                                );
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "[scenario5-request-{index}] child_class={} has_agent_tool={} has_pending_tasks={} has_child_marker={} messages={} tool_uses={tool_use_count} bash_commands={tool_use_commands:?}",
            is_child_request(body),
            tool_names.contains(&"Agent"),
            text.contains("pending_tasks"),
            text.contains(CHILD_MARKER),
            body.get("messages")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0),
        );
    }

    // Construction check: the sub-agent really ran and really owned a shell.
    assert!(outcome.child_steps >= 2, "child model was not exercised");
    assert!(pid.is_some(), "child shell task never started");
    assert_eq!(outcome.stop_reason(), json!("end_turn"));
    // The child shell still sleeps (30s) when the parent ends: the parent's
    // pending count must aggregate child-owned managers instead of reporting a
    // settled run.
    assert_eq!(outcome.pending_tasks(), json!(1));
    assert_eq!(outcome.status(), json!("incomplete"));
    assert_ne!(outcome.exit_code(), Some(0));
    assert_eq!(outcome.stdout_json().unwrap()["pending_tasks"], json!(1));
    if let Some(pid) = pid {
        if alive_after {
            cleanup_pid(pid);
        }
    }
    assert!(
        !alive_after,
        "parent close left the child shell alive (pid {pid:?}); sig={:?}",
        read_trimmed(&child_sig)
    );
}
