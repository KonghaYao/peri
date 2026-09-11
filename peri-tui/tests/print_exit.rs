use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;

const ANSWER: &str = "print-exit-regression-answer";

async fn run_print(format: &str, reject_request: bool, bare: bool) -> std::process::Output {
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

    // Only the provider is mocked: the child runs the real CLI, ACP host,
    // agent loop, notification pump, session close and deployment shutdown.
    let provider = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let (header_end, content_length) = loop {
            let mut buffer = [0; 4096];
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0, "provider request ended before its headers");
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
        assert_eq!(body["stream"], true);

        let (status, content_type, response) = if reject_request {
            (
                "401 Unauthorized",
                "application/json",
                json!({"type": "error", "error": {
                    "type": "authentication_error", "message": "controlled rejection"
                }})
                .to_string(),
            )
        } else {
            let events = [
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
                    json!({"index": 0, "content_block": {
                        "type": "text", "text": ""
                    }}),
                ),
                (
                    "content_block_delta",
                    json!({"index": 0, "delta": {
                        "type": "text_delta", "text": ANSWER
                    }}),
                ),
                ("content_block_stop", json!({"index": 0})),
                (
                    "message_delta",
                    json!({"delta": {"stop_reason": "end_turn"},
                    "usage": {"output_tokens": 1}}),
                ),
                ("message_stop", json!({})),
            ];
            let response = events
                .into_iter()
                .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
                .collect::<String>();
            ("200 OK", "text/event-stream", response)
        };
        socket
            .write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });

    let mut command = Command::new(env!("CARGO_BIN_EXE_peri"));
    command
        .env_clear()
        .env("HOME", fixture.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(fixture.path())
        .args(["--print", "Reply briefly", "--output-format", format])
        .arg("--settings")
        .arg(settings)
        .arg("--db-path")
        .arg(fixture.path().join("threads.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if bare {
        command.arg("--bare");
    }
    let child = command.spawn().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output()).await;
    if result.is_err() {
        provider.abort();
    }
    let output = result
        .expect(
            "print must exit after the provider finishes, even while the ACP pump owns a client",
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), provider)
        .await
        .expect("the CLI must have reached the provider")
        .unwrap();
    output
}

#[tokio::test]
async fn text_answer_exits_process() {
    let output = run_print("text", false, true).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), ANSWER);
}

#[tokio::test]
async fn json_answer_exits_process() {
    let output = run_print("json", false, true).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["content"], ANSWER);
}

#[tokio::test]
async fn streamed_answer_exits_process() {
    let output = run_print("stream-json", false, true).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let chunks = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["type"] == "text")
        .map(|event| event["content"].as_str().unwrap().to_owned())
        .collect::<String>();
    assert_eq!(chunks, ANSWER);
}

#[tokio::test]
async fn provider_error_exits_process_with_failure() {
    let output = run_print("text", true, true).await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("session/prompt"));
}

#[tokio::test]
async fn standard_mode_answer_exits_process() {
    let output = run_print("text", false, false).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), ANSWER);
}
