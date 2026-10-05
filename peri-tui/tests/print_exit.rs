use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use peri_acp_types::interaction::UnansweredCause;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;

const ANSWER: &str = "print-exit-regression-answer";
// AskUserQuestion 场景里问题的选项标签：伪造回答只可能来自它，工具结果不含它
// 才证明确实没有把空答案当回答转述出去。
const ASK_USER_OPTION: &str = "print-exit-regression-option";

#[derive(Clone, Copy)]
enum ProviderScenario {
    Answer,
    Reject,
    ReadFile,
    Truncated,
    RecoverFromTruncation,
    AskUser,
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
    let expected_steps = match scenario {
        ProviderScenario::Answer | ProviderScenario::Reject => 1,
        ProviderScenario::ReadFile
        | ProviderScenario::RecoverFromTruncation
        | ProviderScenario::AskUser => 2,
        ProviderScenario::Truncated => 3,
    };
    let served_steps = Arc::new(AtomicUsize::new(0));
    let provider = tokio::spawn({
        let served_steps = Arc::clone(&served_steps);
        async move {
            let mut step = 0;
            // 请求序列不预先封顶：prediction 调用（见下）与被测 step 序列交织在同一
            // provider 上，服务到测试收摊（`provider.abort()`）为止。step 序列是否
            // 完整由共享计数在子进程退出后断言，不靠「循环自然结束」隐式保证。
            loop {
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
                // 首个 prompt 之后宿主会另发一次 prediction 调用
                // （`peri_acp::host::prediction` → `execute_prediction`）：它不属于本用例的
                // step 序列，但必须被服务——fixture 不应答就把它钉在 provider 上，
                // 会话收摊只能等预测自身超时（30s）或协作宽限（5s）耗尽后强杀，
                // 进程退出因此被顶到预算边缘。
                // 空文本应答让 prediction 动作集为空（不改会话元数据），且不推进 step。
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
                if matches!(scenario, ProviderScenario::AskUser) && step == 1 {
                    assert_ask_user_result_is_unanswered(&body);
                }

                let (status, content_type, response) = if matches!(
                    scenario,
                    ProviderScenario::Reject
                ) {
                    (
                        "401 Unauthorized",
                        "application/json",
                        json!({"type": "error", "error": {
                            "type": "authentication_error", "message": "controlled rejection"
                        }})
                        .to_string(),
                    )
                } else {
                    // step 0 请求工具，step 1 给出终答（工具结果已在上方断言）。
                    let tool_call = match (scenario, step) {
                        (ProviderScenario::ReadFile, 0) => Some((
                            "read-big",
                            // v4：文件工具的模型面名字是 builtin `workspace` 实例的
                            // effective name（裸名 `Read` 的提供面已删除）。
                            "Read",
                            json!({"file_path": big_file}),
                        )),
                        (ProviderScenario::AskUser, 0) => Some((
                            "ask-user-question-1",
                            "AskUserQuestion",
                            json!({"questions": [{
                                "question": "选择部署环境？",
                                "header": "部署环境",
                                "multiSelect": false,
                                "options": [{"label": ASK_USER_OPTION, "description": "fixture 选项"}]
                            }]}),
                        )),
                        _ => None,
                    };
                    let truncate = matches!(scenario, ProviderScenario::Truncated)
                        || (matches!(scenario, ProviderScenario::RecoverFromTruncation)
                            && step == 0);
                    let content = match &tool_call {
                        Some((id, name, _)) => {
                            json!({"type": "tool_use", "id": id, "name": name, "input": {}})
                        }
                        None => json!({"type": "text", "text": ""}),
                    };
                    let delta = match &tool_call {
                        Some((_, _, input)) => {
                            json!({"type": "input_json_delta", "partial_json": input.to_string()})
                        }
                        None => json!({"type": "text_delta", "text": ANSWER}),
                    };
                    let stop_reason = if tool_call.is_some() {
                        "tool_use"
                    } else if truncate {
                        "max_tokens"
                    } else {
                        "end_turn"
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
                            json!({"delta": {"stop_reason": stop_reason},
                    "usage": {"input_tokens": if step == 0 {100} else {500},
                              "cache_read_input_tokens": 20, "cache_creation_input_tokens": 30,
                              "output_tokens": if step == 0 {7} else {11}}}),
                        ),
                        ("message_stop", json!({})),
                    ];
                    let response = sse_frames(&events);
                    ("200 OK", "text/event-stream", response)
                };
                respond(&mut socket, status, content_type, &response).await;
                step += 1;
                served_steps.store(step, Ordering::SeqCst);
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
        .current_dir(fixture.path())
        .args(["--print", "Reply briefly", "--output-format", format])
        // Windows 的 dirs_next::home_dir 不受 HOME/USERPROFILE 覆盖，
        // 启动期的全局配置读取也必须显式指向 fixture。
        .arg("--config-file")
        .arg(&settings)
        .arg("--settings")
        .arg(settings)
        .arg("--db-path")
        .arg(fixture.path().join("threads.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // env_clear 不会自动保留 Windows 系统环境；网络和子进程初始化仍需要
    // 系统目录。只恢复这两个系统变量，临时目录继续由本用例隔离。
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    if bare {
        command.arg("--bare");
    }
    let mut child = command.spawn().unwrap();
    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    // 把 child 和已读输出留在 timeout 外；失败时先 kill/reap，再给出启动/退出
    // 阶段的证据，不能丢掉 wait_with_output 所有权后只剩 Elapsed(())。
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::try_join!(
            child.wait(),
            stdout_pipe.read_to_end(&mut stdout),
            stderr_pipe.read_to_end(&mut stderr),
        )
    })
    .await;
    let status = match result {
        Ok(result) => result.unwrap().0,
        Err(_) => {
            provider.abort();
            let provider_result = provider.await;
            let cleanup = tokio::time::timeout(Duration::from_secs(5), child.kill()).await;
            panic!(
                "print must exit after the provider finishes, even while the ACP pump owns a client; \
                 served steps: {}/{expected_steps}; provider: {provider_result:?}; cleanup: {cleanup:?}\n\
                 stdout:\n{}\nstderr:\n{}",
                served_steps.load(Ordering::SeqCst),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr),
            );
        }
    };
    let output = std::process::Output {
        status,
        stdout,
        stderr,
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while served_steps.load(Ordering::SeqCst) < expected_steps {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the CLI must have reached the provider for every step: 期望 {expected_steps} 次，实到 {} 次; \
             status: {}\nstdout:\n{}\nstderr:\n{}",
            served_steps.load(Ordering::SeqCst),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    });
    provider.abort();
    output
}

/// 把 `(event, data)` 序列渲染成 SSE 帧串（与真实 provider 的 wire 形态一致）。
fn sse_frames(events: &[(&str, Value)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

/// 区分 prediction 调用与用例的 step 序列：prediction 指令由
/// `build_prediction_directive` 生成，`execute_prediction` 把它作为 system 消息注入；
/// Anthropic wire 格式下 system 在顶层 `system` 字段（不在 `messages[0]`），故整体扫描。
fn is_prediction_request(body: &Value) -> bool {
    body.to_string().contains("<prediction_directive>")
}

/// 回一次应答并关闭连接（provider 侧不用 keep-alive）。
async fn respond(socket: &mut tokio::net::TcpStream, status: &str, content_type: &str, body: &str) {
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

/// [回归测试] 第二次真实 provider 请求必须带回诚实的失败工具结果：`is_error=true`、
/// 如实说明客户端无法交互，且不含伪造的空回答（旧行为：`-p` 的 cancel 兜底成空
/// `Answers`，工具把它转述成「回答: 」空串，模型会当成用户已经回答）。
fn assert_ask_user_result_is_unanswered(body: &Value) {
    let tool_result = body["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("messages 必须是数组: {body}"))
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .find(|block| {
            block["type"] == "tool_result" && block["tool_use_id"] == "ask-user-question-1"
        })
        .unwrap_or_else(|| panic!("后续请求必须包含 AskUserQuestion 的 tool_result: {body}"));
    assert_eq!(
        tool_result["is_error"], true,
        "无人可作答必须以失败工具结果上报: {tool_result}"
    );
    let content = tool_result["content"].to_string();
    assert!(
        content.contains(UnansweredCause::NonInteractiveClient.reason_text()),
        "工具结果必须如实说明客户端没有交互界面: {content}"
    );
    assert!(
        !content.contains(ASK_USER_OPTION) && !content.contains("回答: "),
        "工具结果不得携带伪造的回答: {content}"
    );
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
///
/// v4 后文件工具由 builtin `workspace` 实例提供（`Read`）；
/// `--bare` 同样保留这条基础能力路径。
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
    // 事件里的工具名是模型面 effective name（builtin `workspace` 实例），
    // 不是迁移前的裸名 `Read`。
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

/// [回归测试] 真实 `-p` 收到 elicitation 后必须以失败工具结果如实说明「无人可
/// 作答」，不得把 `-p` 的 cancel 兜底成空回答，且进程在限时内自行退出。
///
/// provider 只在第二次请求断言（见 `assert_ask_user_result_is_unanswered`），
/// 格式维度覆盖同一链路的三种输出形态。
#[tokio::test]
async fn ask_user_unanswered_exits_process_without_fabricated_answer() {
    for format in ["text", "json", "stream-json"] {
        let output = run_print(format, ProviderScenario::AskUser, true).await;
        assert!(
            output.status.success(),
            "无人可作答仍须正常结束本轮（{format}）：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        match format {
            "stream-json" => {
                let events: Vec<Value> = text
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                let tool_result = events
                    .iter()
                    .find(|event| {
                        event["type"] == "tool_result" && event["id"] == "ask-user-question-1"
                    })
                    .unwrap_or_else(|| panic!("stream-json 必须输出工具结果: {events:?}"));
                assert!(
                    tool_result["output"]
                        .as_str()
                        .unwrap()
                        .contains(UnansweredCause::NonInteractiveClient.reason_text()),
                    "可见工具结果必须如实说明无法交互: {tool_result}"
                );
                assert_eq!(
                    events.last().unwrap(),
                    &json!({"type": "result", "stop_reason": "end_turn", "status": "completed", "is_error": false, "total_cost_usd": null,
                    "usage": {"input_tokens": 600, "cache_read_input_tokens": 40, "cache_creation_input_tokens": 60, "output_tokens": 18}})
                );
            }
            "json" => {
                let result: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(result["type"], "result");
                assert_eq!(result["content"], ANSWER);
                assert_eq!(result["status"], "completed");
            }
            _ => assert_eq!(text.trim(), ANSWER),
        }
    }
}
