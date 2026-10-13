use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use super::*;
use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::session::LangfuseSession;
use crate::langfuse::session_like::LangfuseSessionLike;
use peri_acp_types::identity::AgentId;
use peri_acp_types::session::TurnTelemetryOutcome;
use peri_agent::agent::LangfuseBridgeLike;
use peri_agent::messages::BaseMessage;
use peri_agent::session::turn::TurnId;
use peri_agent::tools::ToolDefinition;
use peri_config::observability::LangfuseCompression;

// 冻结后的 input 只随来源交给部署，正文不再进 generation body（契约 §3.3），因此正文断言改为
// 在真实链路（真实 Batcher/Session + 本地捕获端点）上读取已上线的 OTLP 字节。

#[tokio::test]
async fn observe_start_reuses_snapshot_and_fallback_keeps_messages_and_tools() {
    let server = CaptureServer::start();
    let session = capture_session(&server).await;
    let tracer = Arc::new(Mutex::new(LangfuseTracer::new(
        session,
        "snapshot-session".to_string(),
        LangfuseConfig::default(),
    )));
    let agent_id = AgentId::new();
    let bridge = LangfuseBridge::new(
        tracer.clone(),
        "provider".to_string(),
        Some(agent_id.to_string()),
    );
    let messages = Arc::new(vec![BaseMessage::human("complete-context🙂".repeat(4096))]);
    let tools = vec![ToolDefinition {
        name: "lookup".to_string(),
        description: "lookup description".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let expected =
        serde_json::to_string(&serde_json::json!({"messages": messages.as_ref(), "tools": &tools}))
            .expect("基线 JSON");
    let event = ObserveEvent::LlmCallStart {
        turn_id: TurnId::new(),
        agent_id,
        step: 0,
        messages: Arc::clone(&messages),
        tools,
    };
    tracer.lock().on_turn_start("input");
    assert_eq!(Arc::strong_count(&messages), 2);
    bridge.process_observe_event(&event);
    // 冻结后到发布前，遥测必须持有原文 Arc（跨事件保留）；此处无 await，worker 尚未收敛。
    assert_eq!(Arc::strong_count(&messages), 3);
    let end = tracer.lock().on_turn_end(TurnTelemetryOutcome::Completed);
    end.await.unwrap();
    // 发布后原文 Arc 必须释放，且 generation 仍拿到完整的 fallback messages/tools 字节。
    assert_eq!(Arc::strong_count(&messages), 2);
    assert_eq!(
        server.generation_input().as_deref(),
        Some(expected.as_str()),
        "generation 必须携带 fallback messages/tools 的冻结字节"
    );
}

#[tokio::test]
async fn observe_raw_request_supersedes_snapshot_for_completed_generation() {
    let server = CaptureServer::start();
    let session = capture_session(&server).await;
    let tracer = Arc::new(Mutex::new(LangfuseTracer::new(
        session,
        "snapshot-session".to_string(),
        LangfuseConfig::default(),
    )));
    let agent_id = AgentId::new();
    let bridge = LangfuseBridge::new(
        tracer.clone(),
        "provider".to_string(),
        Some(agent_id.to_string()),
    );
    let turn_id = TurnId::new();
    let messages = Arc::new(vec![BaseMessage::human("fallback")]);
    let start = ObserveEvent::LlmCallStart {
        turn_id,
        agent_id,
        step: 0,
        messages: Arc::clone(&messages),
        tools: vec![ToolDefinition {
            name: "fallback-tool".to_string(),
            description: "fallback description".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }],
    };
    let raw = serde_json::json!({"messages": [{"content": "native🙂".repeat(4096)}], "tools": [{"name": "native-tool"}], "provider_extra": {"enabled": true}});
    tracer.lock().on_turn_start("input");
    bridge.process_observe_event(&start);
    assert_eq!(Arc::strong_count(&messages), 3);
    bridge.process_observe_event(&ObserveEvent::LlmRequestPayload {
        turn_id,
        agent_id,
        step: 0,
        body: Arc::new(raw.clone()),
    });
    bridge.process_observe_event(&ObserveEvent::LlmCallEnd {
        turn_id,
        agent_id,
        step: 0,
        model: "provider-model".to_string(),
        output: "answer".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
        request_id: None,
    });
    let end = tracer.lock().on_turn_end(TurnTelemetryOutcome::Completed);
    end.await.unwrap();
    // 请求体取代 fallback 后，本轮收敛完成即不得继续保留 fallback 原文 Arc。
    assert_eq!(Arc::strong_count(&messages), 2);
    assert_eq!(
        server.generation_input().as_deref(),
        Some(raw.to_string().as_str()),
        "raw 请求体必须取代 fallback 上线"
    );
}

// ── 本地 fixture：真实部署 session + 本地 OTLP 捕获端点（不共享 test_helpers） ─────────

/// OTLP 中承载观测 input 的属性键。
const INPUT_ATTRIBUTE: &str = "langfuse.observation.input";

/// 捕获 OTLP 正文并回 200 `{}` 的本地端点；只读正文，不解析凭据。
struct CaptureServer {
    url: String,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl CaptureServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind capture server");
        let address = listener.local_addr().expect("capture server address");
        let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&bodies);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                if let Some(body) = read_http_body(&mut stream) {
                    sink.lock().push(body);
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}",
                );
            }
        });
        Self {
            url: format!("http://{address}"),
            bodies,
        }
    }

    /// 已上线的 generation 输入（span 名以 `step-` 开头）；无 generation 时为 `None`。
    fn generation_input(&self) -> Option<String> {
        self.bodies
            .lock()
            .iter()
            .flat_map(|body| otlp_spans(body))
            .find(|span| span.name.starts_with("step-"))
            .and_then(|span| span.attributes.get(INPUT_ATTRIBUTE).cloned())
    }
}

/// 读一个 HTTP 请求的正文（按 `content-length` 截断）。
fn read_http_body(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())?;
    let mut body = buffer[header_end..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Some(body)
}

/// OTLP span 视图：名称与字符串属性。
struct SpanView {
    name: String,
    attributes: HashMap<String, String>,
}

fn otlp_spans(body: &[u8]) -> Vec<SpanView> {
    let value: serde_json::Value = serde_json::from_slice(body).expect("OTLP 正文必须是 JSON");
    let mut spans = Vec::new();
    collect_spans(&value, &mut spans);
    spans
}

fn collect_spans(value: &serde_json::Value, spans: &mut Vec<SpanView>) {
    match value {
        serde_json::Value::Object(object) => {
            let name = object.get("name").and_then(|value| value.as_str());
            let attributes = object.get("attributes").and_then(|value| value.as_array());
            if let (Some(name), Some(attributes)) = (name, attributes) {
                let mut map = HashMap::new();
                for attribute in attributes {
                    let key = attribute.get("key").and_then(|value| value.as_str());
                    let text = attribute
                        .get("value")
                        .and_then(|value| value.get("stringValue"))
                        .and_then(|value| value.as_str());
                    if let (Some(key), Some(text)) = (key, text) {
                        map.insert(key.to_string(), text.to_string());
                    }
                }
                spans.push(SpanView {
                    name: name.to_string(),
                    attributes: map,
                });
            }
            for nested in object.values() {
                collect_spans(nested, spans);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_spans(item, spans);
            }
        }
        _ => {}
    }
}

/// 真实部署 session：捕获端点 + 有界内存路径（无暂存），冻结/编码/HTTP 全走产品实现。
async fn capture_session(server: &CaptureServer) -> Arc<dyn LangfuseSessionLike> {
    let config = LangfuseConfig {
        public_key: Some("pk-capture".into()),
        secret_key: Some("sk-capture".into()),
        host: server.url.clone(),
        trace_sampling: 1.0,
        // 未压缩 OTLP JSON 才能逐字节核对冻结字节；暂存关闭时部署只用内存快照。
        compression: LangfuseCompression::None,
        spool_enabled: false,
        ..Default::default()
    };
    Arc::new(
        LangfuseSession::new(config, "snapshot-session".into(), None)
            .await
            .expect("合法部署配置必须构造出真实 session"),
    )
}
