//! 真实 CLI 回归：后台输出落盘，模型按通知路径读取后自行结束。
#![cfg(unix)]

use std::{path::Path, process::Stdio, time::Duration};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

const OUTPUT_MARKER: &str = "fixture-private-output-正文";
const TAIL: &str = "fixture-complete-output-tail-終点";
const STDERR: &str = "fixture-stderr-diagnostic";
const ANSWER: &str = "background-output-verified";
const OUTPUT_LINES: usize = 7_000;

async fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let (body_start, body_len) = loop {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "provider 请求不应提前结束");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
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
    while bytes.len() < body_start + body_len {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0, "provider 请求 body 不应被截断");
        bytes.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&bytes[body_start..body_start + body_len]).unwrap()
}

async fn respond(socket: &mut TcpStream, tool: Option<(&str, &str, Value)>, text: &str) {
    let (content, delta, stop) = match tool {
        Some((id, name, input)) => (
            json!({"type":"tool_use", "id":id, "name":name, "input":{}}),
            json!({"type":"input_json_delta", "partial_json":input.to_string()}),
            "tool_use",
        ),
        None => (
            json!({"type":"text", "text":""}),
            json!({"type":"text_delta", "text":text}),
            "end_turn",
        ),
    };
    let events = [
        (
            "message_start",
            json!({"message": {"id":"fixture-response", "type":"message", "role":"assistant", "model":"fixture-model", "content":[], "usage":{"input_tokens":1,"output_tokens":0}}}),
        ),
        (
            "content_block_start",
            json!({"index":0, "content_block":content}),
        ),
        ("content_block_delta", json!({"index":0, "delta":delta})),
        ("content_block_stop", json!({"index":0})),
        (
            "message_delta",
            json!({"delta":{"stop_reason":stop}, "usage":{"input_tokens":100,"output_tokens":7}}),
        ),
        ("message_stop", json!({})),
    ];
    let body = events
        .into_iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect::<String>();
    socket
        .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
        .await
        .unwrap();
    socket.shutdown().await.unwrap();
}

fn output_files(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(output_files(&path));
        } else {
            files.push(path);
        }
    }
    files
}

async fn assert_background_output_exits(explicit_background: bool) {
    let fixture = tempfile::tempdir().unwrap();
    let fixture_path = fixture.path().canonicalize().unwrap();
    let output_dir = fixture_path.join("output-artifacts");
    std::fs::create_dir(&output_dir).unwrap();
    let line = format!("{OUTPUT_MARKER} {}\n", "文".repeat(100));
    let payload = format!("{}{TAIL}\n", line.repeat(OUTPUT_LINES));
    assert!(payload.len() > 2 * 1024 * 1024);
    std::fs::write(fixture_path.join("payload.txt"), &payload).unwrap();
    let gate = fixture_path.join("release-shell");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let settings = fixture_path.join("settings.json");
    std::fs::write(&settings, json!({"config":{"providers":[{"id":"fixture", "type":"anthropic", "apiKey":"test-only", "baseUrl":format!("http://{address}"), "models":{"opus":"fixture-model"}}]}}).to_string()).unwrap();
    let provider = tokio::spawn(async move {
        let mut step = 0;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert_eq!(request["stream"], true);
            // 会话标题调用不计入主模型步骤。
            if request["tools"].as_array().is_none_or(Vec::is_empty) {
                respond(&mut socket, None, "fixture title").await;
                continue;
            }
            let messages = request["messages"].to_string();
            match step {
                0 => {
                    let mut input = json!({"command":format!("for attempt in {{1..1000}}; do [ -f release-shell ] && break; sleep 0.01; done; [ -f release-shell ] || exit 70; cat payload.txt; printf '{STDERR}\\n' >&2; exit 1")});
                    if explicit_background {
                        input["run_in_background"] = json!(true);
                    } else {
                        input["timeout"] = json!(1);
                    }
                    respond(&mut socket, Some(("produce-output", "Bash", input)), "").await;
                }
                1 => {
                    let tool_result = request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|message| message["content"].as_array())
                        .flatten()
                        .find(|block| {
                            block["type"] == "tool_result"
                                && block["tool_use_id"] == "produce-output"
                        })
                        .expect("必须收到指定 Bash 调用的真实工具结果");
                    if !explicit_background {
                        assert!(
                            tool_result.to_string().contains("promoted"),
                            "门控进程必须确实走 timeout promotion"
                        );
                    }
                    respond(&mut socket, None, "Waiting for the background result.").await;
                    // 收到工具返回且发出最终回答后才放行真实 shell，无需猜测启动耗时。
                    std::fs::write(&gate, "release").unwrap();
                }
                2 => {
                    assert!(
                        !messages.contains(OUTPUT_MARKER),
                        "reminder 不得注入输出正文"
                    );
                    assert!(!messages.contains(TAIL), "尾部须由后续 Read 显式读取");
                    let files = output_files(&output_dir);
                    let stdout = files
                        .iter()
                        .find(|path| std::fs::read(path).unwrap() == payload.as_bytes())
                        .expect("stdout 文件必须保留超过 2 MiB 的完整 Unicode 内容");
                    let stderr = files
                        .iter()
                        .find(|path| {
                            std::fs::read(path).unwrap() == format!("{STDERR}\n").as_bytes()
                        })
                        .expect("stderr 文件必须完整可读");
                    for path in [stdout, stderr] {
                        assert!(
                            messages.contains(path.to_str().unwrap()),
                            "通知必须携带可读取的绝对路径"
                        );
                    }
                    assert!(messages.contains("Read"), "通知必须明确说明如何读取文件");
                    assert!(
                        messages.contains("执行失败") && messages.contains("退出码 1"),
                        "通知必须保留真实失败状态与退出码"
                    );
                    // 输出文件独立于原始 fixture；随后只能从持久化产物读取结果。
                    std::fs::remove_file(gate.parent().unwrap().join("payload.txt")).unwrap();
                    respond(
                        &mut socket,
                        Some((
                            "read-output-tail",
                            "Read",
                            json!({"file_path":stdout, "offset":OUTPUT_LINES + 1, "limit":1}),
                        )),
                        "",
                    )
                    .await;
                }
                3 => {
                    assert!(
                        messages.contains("read-output-tail") && messages.contains(TAIL),
                        "真实 Read 工具应返回完整文件尾部"
                    );
                    respond(&mut socket, None, ANSWER).await;
                    return;
                }
                _ => unreachable!(),
            }
            step += 1;
        }
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_peri"));
    command
        .env_clear()
        .env("HOME", &fixture_path)
        .env("TMPDIR", fixture_path.join("output-artifacts"))
        .env("TMP", fixture_path.join("output-artifacts"))
        .env("TEMP", fixture_path.join("output-artifacts"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(&fixture_path)
        .args([
            "--print",
            "Run the task and inspect its output tail.",
            "--bare",
            "--output-format",
            "stream-json",
        ])
        .arg("--settings")
        .arg(settings)
        .arg("--db-path")
        .arg(fixture_path.join("threads.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command.spawn().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(25), child.wait_with_output()).await;
    if result.is_err() {
        provider.abort();
    }
    let output = result.expect("后台结果落盘后 print 必须自行退出").unwrap();
    tokio::time::timeout(Duration::from_secs(1), provider)
        .await
        .expect("fixture 应完成真实 Read 回合")
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("panicked"), "后台任务不得 panic：{stderr}");
    let events: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| event["type"] == "tool_result"
        && event["id"] == "read-output-tail"
        && event["output"].as_str().unwrap().contains(TAIL)));
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "text" && event["content"] == ANSWER)
    );
    assert_eq!(events.last().unwrap()["type"], "result");
}

/// [回归测试] timeout promotion 曾因大输出 reminder panic 遗留 Running，最终回答后挂起。
#[tokio::test]
async fn promoted_background_output_is_readable_and_print_exits() {
    assert_background_output_exits(false).await;
}

/// [回归测试] 显式后台路径也应只发送文件引用，并保留完整输出供实际 Read 消费。
#[tokio::test]
async fn explicit_background_output_is_readable_and_print_exits() {
    assert_background_output_exits(true).await;
}
