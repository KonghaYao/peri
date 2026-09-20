use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;

const ANSWER: &str = "print-exit-regression-answer";

#[derive(Clone, Copy)]
enum ProviderScenario {
    Answer,
    Reject,
    ReadFile,
    Truncated,
    RecoverFromTruncation,
}

async fn run_print(format: &str, scenario: ProviderScenario, bare: bool) -> std::process::Output {
    let fixture = tempfile::tempdir().unwrap();
    let big_file = fixture.path().join("big.txt");
    std::fs::write(
        &big_file,
        (1..=60)
            .map(|line| format!("line {line} the quick brown fox jumps over the lazy dog\n"))
            .collect::<String>(),
    )
    .unwrap();
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
        let steps = match scenario {
            ProviderScenario::Answer | ProviderScenario::Reject => 1,
            ProviderScenario::ReadFile | ProviderScenario::RecoverFromTruncation => 2,
            ProviderScenario::Truncated => 3,
        };
        for step in 0..steps {
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
            if matches!(scenario, ProviderScenario::ReadFile) && step == 1 {
                let messages = body["messages"].to_string();
                assert!(
                    messages.contains("line 60 the quick brown fox"),
                    "后续真实请求应包含完整工具返回"
                );
                assert!(
                    messages.contains("tool_result") && messages.contains("read-big"),
                    "保留历史工具调用和结果配对"
                );
            }

            if matches!(scenario, ProviderScenario::RecoverFromTruncation) && step == 1 {
                let messages = body["messages"].to_string();
                assert!(messages.contains(ANSWER), "续跑保留被截断的已生成正文");
                assert!(
                    messages.contains("output token limit"),
                    "后续真实请求包含截断续跑提醒"
                );
            }
            let (status, content_type, response) = if matches!(scenario, ProviderScenario::Reject) {
                (
                    "401 Unauthorized",
                    "application/json",
                    json!({"type": "error", "error": {
                        "type": "authentication_error", "message": "controlled rejection"
                    }})
                    .to_string(),
                )
            } else {
                let call_tool = matches!(scenario, ProviderScenario::ReadFile) && step == 0;
                let truncate = matches!(scenario, ProviderScenario::Truncated)
                    || (matches!(scenario, ProviderScenario::RecoverFromTruncation) && step == 0);
                let content = if call_tool {
                    json!({"type": "tool_use", "id": "read-big", "name": "Read", "input": {}})
                } else {
                    json!({"type": "text", "text": ""})
                };
                let delta = if call_tool {
                    json!({"type": "input_json_delta", "partial_json": json!({"file_path": big_file}).to_string()})
                } else {
                    json!({"type": "text_delta", "text": ANSWER})
                };
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
                        json!({"index": 0, "content_block": content}),
                    ),
                    ("content_block_delta", json!({"index": 0, "delta": delta})),
                    ("content_block_stop", json!({"index": 0})),
                    (
                        "message_delta",
                        json!({"delta": {"stop_reason": if call_tool {"tool_use"} else if truncate {"max_tokens"} else {"end_turn"}},
                    "usage": {"input_tokens": if step == 0 {100} else {500},
                              "cache_read_input_tokens": 20, "cache_creation_input_tokens": 30,
                              "output_tokens": if step == 0 {7} else {11}}}),
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
        }
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
    let output = run_print("text", ProviderScenario::Answer, true).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), ANSWER);
}

#[tokio::test]
async fn json_answer_exits_process() {
    let output = run_print("json", ProviderScenario::Answer, true).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["content"], ANSWER);
    assert_eq!(result["stop_reason"], "end_turn");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["is_error"], false);
}

#[tokio::test]
async fn streamed_answer_exits_process() {
    let output = run_print("stream-json", ProviderScenario::Answer, true).await;
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
    let output = run_print("text", ProviderScenario::Reject, true).await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("session/prompt"));
}

#[tokio::test]
async fn standard_mode_answer_exits_process() {
    let output = run_print("text", ProviderScenario::Answer, false).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), ANSWER);
}

/// [回归测试] 真实 CLI 经 Read 后继续请求，usage 尾帧穿过 ACP 到 stdout，进程自行退出。
#[tokio::test]
async fn streamed_multistep_usage_includes_final_provider_counts_and_exits() {
    let output = run_print("stream-json", ProviderScenario::ReadFile, true).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let calls: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "assistant")
        .collect();
    assert_eq!(calls.len(), 2, "每次模型调用恰好一条 usage");
    for (call, input, completion) in [(calls[0], 100, 7), (calls[1], 500, 11)] {
        assert_eq!(
            call["message"]["usage"],
            json!({"input_tokens": input,
            "cache_read_input_tokens": 20, "cache_creation_input_tokens": 30, "output_tokens": completion})
        );
    }
    assert_ne!(calls[0]["message"]["id"], calls[1]["message"]["id"]);
    assert!(events.iter().any(|event| event["type"] == "tool_use"
        && event["name"] == "Read"
        && event["id"] == "read-big"));
    assert!(events.iter().any(|event| event["type"] == "tool_result"
        && event["id"] == "read-big"
        && event["output"].as_str().unwrap().contains("line 60")));
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "text" && event["content"] == ANSWER)
    );
    assert_eq!(
        events.last().unwrap(),
        &json!({"type": "result", "stop_reason": "end_turn", "status": "completed", "is_error": false, "total_cost_usd": null,
        "usage": {"input_tokens": 600, "cache_read_input_tokens": 40, "cache_creation_input_tokens": 60, "output_tokens": 18}})
    );
}

/// [回归测试] 连续截断耗尽续跑预算必须输出未完成终态，并完成清理后以非零退出。
#[tokio::test]
async fn truncated_answer_exits_process_with_incomplete_result() {
    for format in ["text", "json", "stream-json"] {
        let output = run_print(format, ProviderScenario::Truncated, true).await;
        assert!(!output.status.success(), "输出截断不得退出成功");
        assert!(String::from_utf8_lossy(&output.stderr).contains("max_tokens"));
        if format == "text" {
            assert!(String::from_utf8_lossy(&output.stdout).contains(ANSWER));
            continue;
        }
        let text = String::from_utf8(output.stdout).unwrap();
        let result: Value = if format == "json" {
            serde_json::from_str(&text).unwrap()
        } else {
            serde_json::from_str(text.lines().last().unwrap()).unwrap()
        };
        assert_eq!(result["type"], "result");
        assert_eq!(result["stop_reason"], "max_tokens");
        assert_eq!(result["status"], "incomplete");
        assert_eq!(result["is_error"], true);
        assert!(result.get("cleanup_error").is_none());
    }
}

/// [回归测试] 单次截断经真实 ACP/provider 续跑后完成，不能一律以失败退出。
#[tokio::test]
async fn truncated_answer_recovers_and_exits_process_successfully() {
    let output = run_print("stream-json", ProviderScenario::RecoverFromTruncation, true).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let calls: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "assistant")
        .collect();
    assert_eq!(calls.len(), 2, "一次截断后只需一次续跑");
    let result = events.last().unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["stop_reason"], "end_turn");
    assert_eq!(result["status"], "completed");
    assert_eq!(result["is_error"], false);
    assert!(result.get("cleanup_error").is_none());
}
