//! Responses 真实链路：本地 SSE 服务 → Responses adapter（reqwest transport + SSE 解码）
//! → Agent bridge → SQLite → 下一轮真实请求。
//!
//! 与 `responses_lifecycle_test`（假的 `Model` 实现）互补：这里不 mock adapter，也不
//! 伪造 wire，全部经真实 HTTP。只覆盖必须在本层才能证明的三件事：
//!
//! 1. 不完整响应（`response.incomplete`）不产生可执行工具调用，也不进入重试；
//! 2. SQLite 恢复的原生记录在下一轮真实请求里同源保真回放（密文、参数、call_id、
//!    工具结果各恰好一次）；
//! 3. 跨来源（换 endpoint）降级时请求体不含 reasoning 密文，可见内容与工具配对保留。

use std::sync::Arc;

use peri_model::{OpenAiResponsesConfig, OpenAiResponsesModel, StopReason, TokenUsage};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use url::Url;

use super::{model_bridge::AgentModelBridge, react::ReactLLM};
use crate::{
    agent::stages::{reason::run_reason, ReasonInput, StageContext},
    messages::BaseMessage,
    session::{store::FrozenContext, Session},
    thread::{SqliteThreadStore, ThreadMeta, ThreadStore},
};

const RESPONSES_MODEL: &str = "gpt-5";
const TEST_API_KEY: &str = "test-key";
const ENCRYPTED_REASONING: &str = "CIPHERTEXT-REASONING";
const TOOL_ARGUMENTS: &str = "{\"command\":\"ls -la\"}";

// ─── 本地 HTTP/SSE 服务 ───────────────────────────────────────────────────────

/// 客户端实际发出的请求（head 为原样头部文本，body 为请求体文本）。
struct CapturedRequest {
    head: String,
    body: String,
}

/// 一个脚本化的 SSE 响应。
struct ScriptedResponse {
    request_id: Option<&'static str>,
    body: String,
}

/// 启动只接受 `script.len()` 次连接的本地服务，按序返回脚本响应。
///
/// 返回 base URL（`http://127.0.0.1:{port}/v1/`）与服务任务句柄；任务结束后给出
/// 全部捕获到的请求。
async fn serve_script(
    script: Vec<ScriptedResponse>,
) -> (Url, tokio::task::JoinHandle<Vec<CapturedRequest>>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local test server");
    let port = listener.local_addr().expect("local addr").port();
    let handle = tokio::spawn(async move {
        let mut captured = Vec::new();
        for scripted in script {
            let (mut socket, _) = listener.accept().await.expect("accept");
            captured.push(read_request(&mut socket).await);
            respond(&mut socket, &scripted).await;
        }
        captured
    });
    let endpoint = Url::parse(&format!("http://127.0.0.1:{port}/v1/")).expect("base url");
    (endpoint, handle)
}

async fn read_request(socket: &mut TcpStream) -> CapturedRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        let read = socket.read(&mut chunk).await.expect("read request head");
        assert!(read > 0, "客户端在发送完整请求头前关闭连接");
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let body_len = content_length(&head);
    while buffer.len() < header_end + body_len {
        let read = socket.read(&mut chunk).await.expect("read request body");
        assert!(read > 0, "客户端在发送完整请求体前关闭连接");
        buffer.extend_from_slice(&chunk[..read]);
    }
    CapturedRequest {
        head,
        body: String::from_utf8_lossy(&buffer[header_end..header_end + body_len]).into_owned(),
    }
}

async fn respond(socket: &mut TcpStream, scripted: &ScriptedResponse) {
    let mut head = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n");
    if let Some(request_id) = scripted.request_id {
        head.push_str(&format!("x-request-id: {request_id}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        scripted.body.len()
    ));
    // 客户端可能在中途错误后停止读取：写失败不影响断言，忽略即可。
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(scripted.body.as_bytes()).await;
    let _ = socket.flush().await;
    let _ = socket.shutdown().await;
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0)
}

/// 以标准 SSE 帧编码事件序列。
fn sse(events: Vec<Value>) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!("data: {event}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn request_body(request: &CapturedRequest) -> Value {
    serde_json::from_str(&request.body).expect("request body is json")
}

fn input_items(body: &Value) -> Vec<&Value> {
    body["input"]
        .as_array()
        .expect("input array")
        .iter()
        .collect()
}

fn items_of_type<'a>(items: &[&'a Value], item_type: &str) -> Vec<&'a Value> {
    items
        .iter()
        .copied()
        .filter(|item| item["type"] == item_type)
        .collect()
}

// ─── 响应载荷 ────────────────────────────────────────────────────────────────

/// reasoning（含密文）+ message + function_call 的完成响应原生 items。
fn completed_output_items() -> Vec<Value> {
    vec![
        json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{"type": "summary_text", "text": "先想一步"}],
            "encrypted_content": ENCRYPTED_REASONING,
            "status": "completed",
        }),
        json!({
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "答案正文"}],
        }),
        json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "shell",
            "arguments": TOOL_ARGUMENTS,
            "status": "completed",
        }),
    ]
}

fn completed_response(
    request_id: &'static str,
    output: Vec<Value>,
    usage: Value,
) -> ScriptedResponse {
    ScriptedResponse {
        request_id: Some(request_id),
        body: sse(vec![
            json!({
                "type": "response.created",
                "response": {"id": "resp_created", "status": "in_progress"},
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_1",
                    "status": "completed",
                    "output": output,
                    "usage": usage,
                },
            }),
        ]),
    }
}

fn adapter(endpoint: &Url, model: &str) -> OpenAiResponsesModel {
    OpenAiResponsesModel::new(OpenAiResponsesConfig::new(
        endpoint.clone(),
        TEST_API_KEY,
        model,
    ))
}

// ─── 测试 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn real_responses_executes_tool_once_and_continues_through_react_loop() {
    use crate::{
        agent::stages::{run_react_loop, LoopResult, SharedToolMap},
        session::{MessageSource, QueuedMessage},
    };
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    struct LocalTool(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl crate::tools::BaseTool for LocalTool {
        fn name(&self) -> &str {
            "shell"
        }
        fn description(&self) -> &str {
            "无副作用的本地测试工具"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {"command": {"type": "string"}}})
        }
        async fn invoke(
            &self,
            input: Value,
            _: crate::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if input["command"] == "fail" {
                Err(std::io::Error::other("local tool failure").into())
            } else {
                assert_eq!(input["command"], "ls -la");
                Ok("local tool result".into())
            }
        }
    }
    let mut output = completed_output_items();
    output.push(json!({"type": "function_call", "id": "fc_2", "call_id": "call_2", "name": "shell", "arguments": "{\"command\":\"fail\"}", "status": "completed"}));
    let mut tool_response = completed_response(
        "req_tool",
        output,
        json!({"input_tokens": 20, "output_tokens": 10}),
    );
    let deltas = sse(vec![
        json!({"type": "response.output_item.added", "output_index": 2, "item": {"type": "function_call", "call_id": "call_1", "name": "shell"}}),
        json!({"type": "response.output_item.added", "output_index": 3, "item": {"type": "function_call", "call_id": "call_2", "name": "shell"}}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 3, "delta": "{\"command\":"}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 2, "delta": TOOL_ARGUMENTS}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 3, "delta": "\"fail\"}"}),
    ]);
    tool_response.body =
        deltas.trim_end_matches("data: [DONE]\n\n").to_owned() + &tool_response.body;
    let (endpoint, server) = serve_script(vec![
        ScriptedResponse {
            request_id: None,
            body: sse(vec![json!({"type": "error", "code": "server_error"})]),
        },
        tool_response,
        completed_response(
            "req_final",
            vec![json!({
                "type": "message", "id": "msg_final", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": "最终答案", "annotations": []}],
            })],
            json!({"input_tokens": 30, "output_tokens": 5}),
        ),
    ])
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let tools: SharedToolMap = Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "shell".to_owned(),
        Arc::new(LocalTool(calls.clone())) as Arc<dyn crate::tools::BaseTool>,
    )])));
    let directory = tempfile::tempdir().unwrap();
    let session = Session::new(
        Arc::from(directory.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(AgentModelBridge::new(Arc::new(adapter(
        &endpoint,
        RESPONSES_MODEL,
    )))))
    .with_tools(tools)
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("执行本地测试工具"),
    ));
    let result = tokio::time::timeout(Duration::from_secs(10), run_react_loop(context.clone(), 4))
        .await
        .unwrap();
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let requests = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(request_body(&requests[0]), request_body(&requests[1]));
    let body = request_body(&requests[2]);
    let input = input_items(&body);
    let outputs = items_of_type(&input, "function_call_output");
    assert_eq!(outputs.len(), 2);
    let failure = outputs
        .iter()
        .find(|output| output["call_id"] == "call_2")
        .unwrap();
    assert!(failure["output"]
        .as_str()
        .unwrap()
        .contains("Tool execution failed"));
    assert!(failure["output"]
        .as_str()
        .unwrap()
        .contains("local tool failure"));
    assert_eq!(outputs[0]["call_id"], "call_1");
    assert!(outputs[0]["output"]
        .as_str()
        .unwrap()
        .contains("local tool result"));
    assert_eq!(items_of_type(&input, "function_call").len(), 2);
    assert_eq!(
        items_of_type(&input, "reasoning")[0]["encrypted_content"],
        ENCRYPTED_REASONING
    );
    assert!(context
        .session
        .transcript
        .read()
        .visible_messages()
        .iter()
        .any(|message| message.content().contains("最终答案")));
}

/// 不完整响应（provider 明确返回 `response.incomplete`，参数已流式过半）必须在
/// adapter 层失败：既不产出 `Completed`，也不产生可执行工具调用，且不进入重试。
#[tokio::test]
async fn responses_http_cancel_after_tool_delta_never_completes_or_retries() {
    use futures::StreamExt;
    use peri_model::{Model, ModelRequest, ModelStreamEvent};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Url::parse(&format!("http://{}/v1", listener.local_addr().unwrap())).unwrap();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _request = read_request(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let partial = sse(vec![
            json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call", "call_id": "call_1", "name": "shell"}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 0, "delta": TOOL_ARGUMENTS}),
        ]);
        socket
            .write_all(partial.trim_end_matches("data: [DONE]\n\n").as_bytes())
            .await
            .unwrap();
        let _ = finish_rx.await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    });
    let cancellation = tokio_util::sync::CancellationToken::new();
    let mut stream = adapter(&endpoint, RESPONSES_MODEL)
        .stream(ModelRequest::default(), cancellation.clone())
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::ToolCallDelta { .. }))
    ));
    cancellation.cancel();
    let remaining = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        stream.collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    assert!(!remaining
        .iter()
        .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    finish_tx.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn responses_http_usage_preserves_missing_and_zero_on_event_boundary() {
    use crate::agent::events_v2::{EventBus, EventBusConfig, ObserveEvent};
    // 整体 usage 缺失、显式零与真实数值必须在消费端保持可区分，cache 明细同理。
    for (usage, expected) in [
        (None, None),
        (
            Some(json!({"input_tokens": 0, "output_tokens": 0})),
            Some((0, 0, None)),
        ),
        (
            Some(json!({"input_tokens": 20, "output_tokens": 5})),
            Some((20, 5, None)),
        ),
        (
            Some(
                json!({"input_tokens": 20, "output_tokens": 5, "input_tokens_details": {"cached_tokens": 0}}),
            ),
            Some((20, 5, Some(0))),
        ),
        (
            Some(
                json!({"input_tokens": 20, "output_tokens": 5, "input_tokens_details": {"cached_tokens": 7}}),
            ),
            Some((20, 5, Some(7))),
        ),
    ] {
        let mut response_json = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_1",
                "status": "completed",
                "output": completed_output_items(),
            },
        });
        if let Some(usage) = usage {
            response_json["response"]["usage"] = usage;
        }
        let response = ScriptedResponse {
            request_id: Some("req_usage"),
            body: sse(vec![response_json]),
        };
        let (endpoint, server) = serve_script(vec![response]).await;
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let session = Session::new(
            Arc::from("/tmp/responses-http"),
            FrozenContext::builder().build(),
            None,
        );
        let context = StageContext::builder(
            session.start_turn(),
            session.transcript(),
            session.queue().clone(),
        )
        .with_llm(Arc::new(AgentModelBridge::new(Arc::new(adapter(
            &endpoint,
            RESPONSES_MODEL,
        )))))
        .with_event_bus(Arc::new(bus))
        .build();
        run_reason(ReasonInput {
            context,
            has_tool_calls: false,
        })
        .await
        .unwrap();
        let mut usage_events = 0;
        while let Some(event) = handles.try_observe() {
            if matches!(event, ObserveEvent::LlmCallEnd { .. }) {
                let mapped = peri_acp_types::event_v2::observe_event_to_executor(event).unwrap();
                let peri_acp_types::event::ExecutorEvent::LlmCallEnd { usage, .. } = mapped else {
                    panic!("LLM event expected")
                };
                match (usage, expected) {
                    (None, None) => {}
                    (Some(usage), Some((input, output, cache_read))) => {
                        assert_eq!(usage.input_tokens, input);
                        assert_eq!(usage.output_tokens, output);
                        assert_eq!(usage.cache_read_input_tokens, cache_read);
                    }
                    (usage, expected) => panic!("usage 语义被折叠: {usage:?} vs {expected:?}"),
                }
                usage_events += 1;
            }
        }
        assert_eq!(usage_events, 1);
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

/// 计数工具：任何一次真实执行都会递增（用于证明未确认的工具调用不执行）。
struct CountingTool(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl crate::tools::BaseTool for CountingTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "计数用本地工具"
    }

    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"command": {"type": "string"}}})
    }

    async fn invoke(
        &self,
        _input: Value,
        _context: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok("executed".into())
    }
}

fn counting_tools(
    calls: Arc<std::sync::atomic::AtomicUsize>,
) -> crate::agent::stages::SharedToolMap {
    use std::collections::BTreeMap;
    Arc::new(parking_lot::RwLock::new(BTreeMap::from([(
        "shell".to_owned(),
        Arc::new(CountingTool(calls)) as Arc<dyn crate::tools::BaseTool>,
    )])))
}

/// `response.failed` 携 usage → 按策略重试 → 成功：两次尝试的真实用量必须
/// 各保留一次（累加、不重复、不丢失），未确认的工具调用不得执行。
#[tokio::test]
async fn responses_failed_with_usage_retries_and_keeps_each_attempt_usage() {
    use crate::agent::events_v2::{EventBus, EventBusConfig, ObserveEvent};
    use crate::agent::stages::{run_react_loop, LoopResult};
    use crate::session::{MessageSource, QueuedMessage};

    let (endpoint, server) = serve_script(vec![
        ScriptedResponse {
            request_id: Some("req_failed"),
            body: sse(vec![json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_failed",
                    "status": "failed",
                    "error": {"code": "server_error"},
                    "usage": {"input_tokens": 30, "output_tokens": 2},
                },
            })]),
        },
        ScriptedResponse {
            request_id: Some("req_final"),
            body: sse(vec![json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_final",
                    "status": "completed",
                    "output": [{
                        "type": "message", "id": "msg_final", "role": "assistant", "status": "completed",
                        "content": [{"type": "output_text", "text": "最终答案"}],
                    }],
                    "usage": {"input_tokens": 11, "output_tokens": 4},
                },
            })]),
        },
    ])
    .await;

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let directory = tempfile::tempdir().unwrap();
    let session = Session::new(
        Arc::from(directory.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(AgentModelBridge::new(Arc::new(adapter(
        &endpoint,
        RESPONSES_MODEL,
    )))))
    .with_tools(counting_tools(Arc::clone(&calls)))
    .with_event_bus(Arc::new(bus))
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("回答问题"),
    ));

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_react_loop(context, 4),
    )
    .await
    .unwrap();
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "未确认的工具调用不得执行"
    );

    let mut llm_end = 0;
    let mut usage = None;
    while let Some(event) = handles.try_observe() {
        if let ObserveEvent::LlmCallEnd {
            usage: call_usage, ..
        } = event
        {
            usage = call_usage;
            llm_end += 1;
        }
    }
    assert_eq!(llm_end, 1, "一次调用只上报一次用量");
    let usage = usage.expect("失败尝试的用量不得被终态去重删除");
    assert_eq!(
        usage.input_tokens, 41,
        "30（失败尝试）+ 11（成功尝试），各一次"
    );
    assert_eq!(usage.output_tokens, 6, "2 + 4，各一次");

    let requests = tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requests.len(), 2, "暂态失败必须真实重试");
}

/// `response.incomplete` 携 usage：非成功、不重试、不执行工具，但已发生的
/// 真实用量必须上报，且不得伪造成显式零。
#[tokio::test]
async fn responses_incomplete_with_usage_never_retries_and_never_executes_tools() {
    use crate::agent::events_v2::{EventBus, EventBusConfig, ObserveEvent};
    use crate::agent::stages::{run_react_loop, LoopResult};
    use crate::session::{MessageSource, QueuedMessage};

    let (endpoint, server) = serve_script(vec![ScriptedResponse {
        request_id: Some("req_incomplete"),
        body: sse(vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {"type": "function_call", "call_id": "call_1", "name": "shell"},
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": TOOL_ARGUMENTS,
            }),
            json!({
                "type": "response.incomplete",
                "response": {
                    "id": "resp_incomplete",
                    "status": "incomplete",
                    "incomplete_details": {"reason": "max_output_tokens"},
                    "usage": {"input_tokens": 17, "output_tokens": 3},
                },
            }),
        ]),
    }])
    .await;

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (bus, mut handles) = EventBus::new(EventBusConfig::default());
    let directory = tempfile::tempdir().unwrap();
    let session = Session::new(
        Arc::from(directory.path().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    );
    let context = StageContext::builder(
        session.start_turn(),
        session.transcript(),
        session.queue().clone(),
    )
    .with_llm(Arc::new(AgentModelBridge::new(Arc::new(adapter(
        &endpoint,
        RESPONSES_MODEL,
    )))))
    .with_tools(counting_tools(Arc::clone(&calls)))
    .with_event_bus(Arc::new(bus))
    .build();
    context.session.queue.push(QueuedMessage::prompt(
        MessageSource::UserInput,
        BaseMessage::human("执行本地工具"),
    ));

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_react_loop(context.clone(), 4),
    )
    .await
    .unwrap();
    assert!(
        matches!(result, LoopResult::Error(_)),
        "incomplete 必须是非成功终态"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "不完整响应携带的工具调用不得执行"
    );
    assert!(
        context
            .session
            .transcript
            .read()
            .visible_messages()
            .iter()
            .all(|message| message.tool_calls().is_empty()),
        "不完整响应不得进入 transcript 的可执行工具调用"
    );

    let mut llm_end = 0;
    let mut usage = None;
    while let Some(event) = handles.try_observe() {
        if let ObserveEvent::LlmCallEnd {
            usage: call_usage, ..
        } = event
        {
            usage = call_usage;
            llm_end += 1;
        }
    }
    assert_eq!(llm_end, 1);
    let usage = usage.expect("incomplete 已发生的用量必须如实上报");
    assert_eq!(usage.input_tokens, 17);
    assert_eq!(usage.output_tokens, 3);

    let requests = tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requests.len(), 1, "incomplete 是稳定失败：不得重试");
}

#[tokio::test]
async fn responses_incomplete_safe_reason_reaches_turn_error() {
    use crate::agent::events_v2::{EventBus, EventBusConfig, ObserveEvent, TurnErrorReason};
    for (reason, expected) in [
        ("max_output_tokens", "incomplete.output_limit"),
        ("content_filter", "incomplete.content_filter"),
        ("private-provider-detail", "incomplete"),
    ] {
        let (endpoint, server) = serve_script(vec![ScriptedResponse {
            request_id: None,
            body: sse(vec![json!({"type": "response.incomplete", "response": {
                "incomplete_details": {"reason": reason},
                "output": completed_output_items(),
            }})]),
        }])
        .await;
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let session = Session::new(
            Arc::from("/tmp/responses-http"),
            FrozenContext::builder().build(),
            None,
        );
        let context = StageContext::builder(
            session.start_turn(),
            session.transcript(),
            session.queue().clone(),
        )
        .with_llm(Arc::new(AgentModelBridge::new(Arc::new(adapter(
            &endpoint,
            RESPONSES_MODEL,
        )))))
        .with_event_bus(Arc::new(bus))
        .build();
        assert!(run_reason(ReasonInput {
            context: context.clone(),
            has_tool_calls: false
        })
        .await
        .is_err());
        let mut errors = Vec::new();
        while let Some(event) = handles.try_observe() {
            if let ObserveEvent::TurnError {
                reason, message, ..
            } = event
            {
                assert_eq!(reason, TurnErrorReason::LlmFailure);
                assert!(message.contains(expected));
                assert!(!message.contains("private-provider-detail"));
                errors.push(message);
            }
        }
        assert_eq!(errors.len(), 1);
        assert!(context
            .session
            .transcript
            .read()
            .visible_messages()
            .iter()
            .all(|message| message.tool_calls().is_empty()));
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn incomplete_response_never_yields_executable_tool_calls() {
    let (endpoint, server) = serve_script(vec![ScriptedResponse {
        request_id: Some("req_incomplete"),
        body: sse(vec![
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {"type": "function_call", "call_id": "call_1", "name": "shell"},
            }),
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": "{\"command\":\"rm -rf /\"}",
            }),
            json!({
                "type": "response.incomplete",
                "response": {"id": "resp_1", "status": "incomplete"},
            }),
        ]),
    }])
    .await;

    let cwd: Arc<str> = Arc::from("/tmp/responses-http");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let llm: Arc<dyn ReactLLM + Send + Sync> = Arc::new(AgentModelBridge::new(Arc::new(adapter(
        &endpoint,
        RESPONSES_MODEL,
    ))));
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone())
        .with_llm(llm)
        .build();

    let result = run_reason(ReasonInput {
        context: ctx.clone(),
        has_tool_calls: false,
    })
    .await;

    assert!(result.is_err(), "不完整响应必须让 Reason 阶段失败");
    {
        let transcript = ctx.session.transcript.read();
        assert!(
            transcript
                .visible_messages()
                .iter()
                .all(|message| message.tool_calls().is_empty()),
            "不完整响应不得进入 transcript 的可执行工具调用"
        );
    }

    let requests = server.await.expect("server task");
    assert_eq!(
        requests.len(),
        1,
        "不完整响应是稳定失败：不得重试（否则会产生重复的不可执行调用）"
    );
    assert!(
        requests[0].head.starts_with("POST /v1/responses"),
        "真实请求必须打到 responses endpoint，实际：{}",
        requests[0].head.lines().next().unwrap_or_default()
    );
}

/// 第一轮真实 HTTP 完成 → 真实 SQLite → 第二轮真实 HTTP：同源记录逐项回放，
/// 密文/文本/工具调用/工具结果各恰好一次，wire 上不带凭据。
#[tokio::test]
async fn persisted_history_replays_natively_on_real_wire() {
    let (endpoint, server) = serve_script(vec![
        completed_response(
            "req_1",
            completed_output_items(),
            json!({
                "input_tokens": 120,
                "output_tokens": 45,
                "input_tokens_details": {"cached_tokens": 7},
            }),
        ),
        completed_response(
            "req_2",
            vec![json!({
                "type": "message",
                "id": "msg_2",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "后续回答"}],
            })],
            json!({"input_tokens": 10, "output_tokens": 3}),
        ),
    ])
    .await;

    // 第一轮：真实 HTTP + 真实 SQLite
    let bridge = AgentModelBridge::new(Arc::new(adapter(&endpoint, RESPONSES_MODEL)));
    let reasoning = bridge
        .generate_reasoning(&[BaseMessage::human("调用工具看看")], &[], None)
        .await
        .expect("completed response reasoning");
    assert_eq!(reasoning.request_id.as_deref(), Some("req_1"));
    assert_eq!(reasoning.stop_reason, StopReason::ToolUse);
    assert_eq!(
        reasoning.usage,
        Some(TokenUsage {
            input_tokens: 120,
            output_tokens: 45,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(7),
        })
    );
    let source_message = reasoning.source_message.expect("source message");
    assert_eq!(source_message.tool_calls()[0].id, "call_1");

    let (_dir, _store, _thread, loaded) = reload_through_store(source_message).await;

    // 第二轮：同一 endpoint/model（同来源）→ 记录必须逐字回放
    let follow_up = AgentModelBridge::new(Arc::new(adapter(&endpoint, RESPONSES_MODEL)));
    follow_up
        .generate_reasoning(&loaded, &[], None)
        .await
        .expect("follow-up turn");

    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 2, "两轮各一次请求");
    let body = request_body(&requests[1]);
    assert_eq!(body["model"], json!(RESPONSES_MODEL));
    assert_eq!(body["store"], json!(false));

    let items = input_items(&body);
    let reasoning_items = items_of_type(&items, "reasoning");
    assert_eq!(reasoning_items.len(), 1, "推理记录必须恰好回放一次");
    assert_eq!(
        reasoning_items[0]["encrypted_content"],
        json!(ENCRYPTED_REASONING),
        "同源回放必须携带记录密文（wire 事实，本地 Debug/遥测仍脱敏）"
    );

    let messages = items_of_type(&items, "message");
    assert_eq!(messages.len(), 1, "可见文本必须恰好回放一次");
    assert_eq!(messages[0]["content"][0]["text"], json!("答案正文"));

    let calls = items_of_type(&items, "function_call");
    assert_eq!(calls.len(), 1, "工具调用不得重复");
    assert_eq!(calls[0]["call_id"], json!("call_1"));
    assert_eq!(
        calls[0]["arguments"],
        json!(TOOL_ARGUMENTS),
        "工具参数必须字节保真"
    );

    let outputs = items_of_type(&items, "function_call_output");
    assert_eq!(outputs.len(), 1, "工具结果不得重复");
    assert_eq!(outputs[0]["call_id"], json!("call_1"));
    assert!(
        outputs[0]["output"]
            .as_str()
            .unwrap_or_default()
            .contains("总用量 12"),
        "工具结果内容必须保真"
    );

    // 顺序与原生记录一致（推理 → 文本 → 调用 → 结果）
    let types: Vec<&str> = items
        .iter()
        .filter_map(|item| item["type"].as_str())
        .filter(|kind| {
            matches!(
                *kind,
                "reasoning" | "message" | "function_call" | "function_call_output"
            )
        })
        .collect();
    assert_eq!(
        types,
        vec![
            "reasoning",
            "message",
            "function_call",
            "function_call_output"
        ]
    );

    // 凭据只存在于 transport 层头部，绝不进入请求体
    assert!(
        requests[1]
            .head
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {TEST_API_KEY}")),
        "认证必须走 transport 头部"
    );
    assert!(
        !requests[1].body.contains(TEST_API_KEY),
        "请求体不得携带凭据"
    );
}

/// 换来源（不同 endpoint = 不同端口）后，原生记录降级为通用 message/function_call：
/// 可见内容与工具配对保留，reasoning 密文绝不跨来源回放。
#[tokio::test]
async fn foreign_source_history_degrades_without_ciphertext_on_wire() {
    let (origin_endpoint, origin_server) = serve_script(vec![completed_response(
        "req_origin",
        completed_output_items(),
        json!({"input_tokens": 120, "output_tokens": 45}),
    )])
    .await;

    let bridge = AgentModelBridge::new(Arc::new(adapter(&origin_endpoint, RESPONSES_MODEL)));
    let reasoning = bridge
        .generate_reasoning(&[BaseMessage::human("调用工具看看")], &[], None)
        .await
        .expect("origin turn");
    let (_dir, _store, _thread, loaded) =
        reload_through_store(reasoning.source_message.expect("source message")).await;
    origin_server.await.expect("origin server task");

    // 另一台服务器 = 另一来源（endpoint 不同）
    let (other_endpoint, other_server) = serve_script(vec![completed_response(
        "req_other",
        vec![json!({
            "type": "message",
            "id": "msg_2",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "后续回答"}],
        })],
        json!({"input_tokens": 10, "output_tokens": 3}),
    )])
    .await;

    let other = AgentModelBridge::new(Arc::new(adapter(&other_endpoint, RESPONSES_MODEL)));
    other
        .generate_reasoning(&loaded, &[], None)
        .await
        .expect("cross-source follow-up");

    let requests = other_server.await.expect("other server task");
    let body = request_body(&requests[0]);
    let wire = serde_json::to_string(&body).expect("body json");
    assert!(
        !wire.contains(ENCRYPTED_REASONING),
        "跨来源降级不得携带记录密文"
    );

    let items = input_items(&body);
    assert!(
        items
            .iter()
            .all(|item| item.get("encrypted_content").is_none()),
        "跨来源请求体不得回放 reasoning 密文字段"
    );
    assert!(
        items_of_type(&items, "reasoning").is_empty(),
        "跨来源不得回放 reasoning 记录"
    );

    let messages = items_of_type(&items, "message");
    assert_eq!(messages.len(), 1, "可见文本仍必须回放");
    assert_eq!(messages[0]["content"][0]["text"], json!("答案正文"));

    let calls = items_of_type(&items, "function_call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["call_id"], json!("call_1"));
    assert_eq!(calls[0]["arguments"], json!(TOOL_ARGUMENTS));
    let outputs = items_of_type(&items, "function_call_output");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0]["call_id"], json!("call_1"));
}

/// 真实 SQLite 往返：写入 AI 消息与工具结果后重新加载。
async fn reload_through_store(
    source_message: BaseMessage,
) -> (
    tempfile::TempDir,
    Arc<dyn ThreadStore>,
    String,
    Vec<BaseMessage>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("responses-http.db"))
            .await
            .expect("sqlite store"),
    );
    let thread_id = store
        .create_thread(ThreadMeta::new("/tmp"))
        .await
        .expect("thread");
    store
        .append_message(&thread_id, source_message)
        .await
        .expect("persist assistant");
    store
        .append_message(&thread_id, BaseMessage::tool_result("call_1", "总用量 12"))
        .await
        .expect("persist tool result");
    let loaded = store.load_messages(&thread_id).await.expect("load history");
    (dir, store, thread_id, loaded)
}
