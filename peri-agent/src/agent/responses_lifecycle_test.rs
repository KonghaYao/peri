//! Responses 原生历史跨层生命周期：Agent bridge 双向 + 真实 SQLite 持久化 +
//! Responses 记录契约与观测出口。
//!
//! 目标链路：Responses 完成响应 → `Reasoning`/`BaseMessage`（含原生记录与派生视图）
//! → SQLite 保存/加载 → 下一轮 `ModelRequest`。断言记录保真、同源可回放、跨来源降级
//! 不带密文、工具参数与 call_id 无重复，以及 usage/request_id 不丢失。
//!
//! 观测出口断言基于 `PreparedModelRequest`：它是唯一发往遥测的请求投影，敏感键
//! （reasoning 密文、function_call.arguments）必须始终脱敏。

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use futures::stream;
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    ModelStreamEvent, OpenAiResponsesConfig, OpenAiResponsesModel, PreparedModelRequest,
    ProviderProtocol, ResponsesHistoryV1, ResponsesSourceIdentity, StopReason, TokenUsage,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{model_bridge::AgentModelBridge, react::ReactLLM};
use crate::{
    messages::{BaseMessage, ContentBlock, MessageContent},
    thread::{SqliteThreadStore, ThreadMeta, ThreadStore},
};

const RESPONSES_BASE: &str = "https://api.example.com/v1/";
const RESPONSES_MODEL: &str = "gpt-5";
const ENCRYPTED_REASONING: &str = "CIPHERTEXT-REASONING";
const TOOL_ARGUMENTS: &str = "{\"command\":\"ls -la\"}";

/// adapter 实际请求的 endpoint（`{base}/responses`）；来源身份以它为准。
fn responses_endpoint() -> Url {
    let mut endpoint = Url::parse(RESPONSES_BASE).expect("valid base url");
    endpoint.set_path("/v1/responses");
    endpoint
}

fn responses_base() -> Url {
    Url::parse(RESPONSES_BASE).expect("valid base url")
}

/// 一次完成的 Responses output：reasoning（含密文）+ message + function_call。
fn responses_items() -> Vec<serde_json::Value> {
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

fn sample_history() -> ResponsesHistoryV1 {
    let source = ResponsesSourceIdentity::capture(&responses_endpoint(), RESPONSES_MODEL)
        .expect("source capture");
    ResponsesHistoryV1::new(source, responses_items()).expect("valid history")
}

/// 返回 Responses 形状完成的模型：派生视图与原生记录同源（与 adapter 输出一致）。
struct ResponsesResponseModel;

#[async_trait]
impl Model for ResponsesResponseModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_reasoning: true,
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    fn prepare_request(&self, _request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        PreparedModelRequest::observe(
            ProviderProtocol::OpenAiResponses,
            RESPONSES_MODEL,
            responses_endpoint(),
            json!({}),
            BTreeMap::new(),
        )
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        let history = sample_history();
        let content = vec![
            peri_model::ContentBlock::reasoning(history.reasoning_summary_text()),
            peri_model::ContentBlock::text(history.visible_text()),
            peri_model::ContentBlock::ResponsesNativeHistory {
                history: Box::new(history.clone()),
            },
        ];
        let response = ModelResponse::new(
            ModelMessage::assistant(content, history.tool_calls()),
            StopReason::ToolUse,
            Some(TokenUsage {
                input_tokens: 123,
                output_tokens: 45,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(7),
            }),
            Some("req_responses_1".into()),
        )?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

/// 真实 adapter 负责投影；测试仅替换传输，避免访问线上端点。
struct ProjectionModel(Arc<dyn Model>);

#[async_trait]
impl Model for ProjectionModel {
    fn capabilities(&self) -> ModelCapabilities {
        self.0.capabilities()
    }
    fn prepare_request(&self, request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        self.0.prepare_request(request)
    }
    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        let body = self.prepare_request(&request)?;
        assert!(!body
            .body()
            .as_value()
            .to_string()
            .contains("模型来源已变化"));
        let response = ModelResponse::new(
            ModelMessage::assistant_text("done"),
            StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

#[tokio::test]
async fn responses_source_warning_is_safe_once_per_request_and_not_context() {
    use crate::agent::{
        events_v2::{EventBus, EventBusConfig, StateEvent},
        react::StreamingContext,
    };
    let (_dir, mut messages) = persisted_messages().await;
    // 两条来源相同的历史也只通知一次。
    messages.extend(messages.clone());
    let models: Vec<(Arc<dyn Model>, bool)> = vec![
        (Arc::new(responses_adapter(RESPONSES_MODEL)), false),
        (Arc::new(responses_adapter("different-model")), true),
        (
            Arc::new(OpenAiResponsesModel::new(OpenAiResponsesConfig::new(
                Url::parse("https://other.example.test/v1").unwrap(),
                "test-key",
                RESPONSES_MODEL,
            ))),
            true,
        ),
        (
            Arc::new(peri_model::OpenAiModel::new(peri_model::OpenAiConfig::new(
                responses_base(),
                "test-key",
                "chat",
            ))),
            true,
        ),
        (
            Arc::new(peri_model::AnthropicModel::new(
                peri_model::AnthropicConfig::new(responses_base(), "test-key", "anthropic"),
            )),
            true,
        ),
    ];
    let original = serde_json::to_value(&messages).unwrap();
    for (model, degraded) in models {
        let bridge = AgentModelBridge::new(Arc::new(ProjectionModel(model)));
        let (bus, mut handles) = EventBus::new(EventBusConfig::default());
        let context = StreamingContext {
            event_bus: Arc::new(bus),
            turn_id: crate::session::turn::TurnId::new(),
            agent_id: peri_acp_types::identity::AgentId::new(),
            cancel: CancellationToken::new(),
        };
        for observed in [false, true] {
            if observed {
                bridge
                    .generate_reasoning_with_observed_body(&messages, &[], Some(context.clone()))
                    .await
                    .unwrap();
            } else {
                bridge
                    .generate_reasoning(&messages, &[], Some(context.clone()))
                    .await
                    .unwrap();
            }
            if degraded {
                let event = handles.try_state().expect("降级通知");
                let StateEvent::ProtocolEvent {
                    turn_id,
                    agent_id,
                    event,
                } = event
                else {
                    panic!("应走状态协议事件")
                };
                assert_eq!(turn_id, context.turn_id);
                assert_eq!(agent_id, context.agent_id);
                let peri_acp_types::event::ExecutorEvent::SystemNotification { text, level } =
                    event
                else {
                    panic!("应为系统通知")
                };
                assert_eq!(level, "warning");
                assert_eq!(
                    text,
                    "模型来源已变化：保留通用对话和工具历史，不回放原生推理。"
                );
            }
            assert!(handles.try_state().is_none(), "每请求只能通知一次");
        }
    }
    assert_eq!(serde_json::to_value(&messages).unwrap(), original);
    // 切回原 Responses 来源时仍能恢复原生回放；跨协议投影未改写 canonical 历史。
    let restored = observed_requests_body(&messages, RESPONSES_MODEL);
    assert_eq!(
        restored["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "reasoning")
            .count(),
        2
    );
}

fn responses_adapter(model: &str) -> OpenAiResponsesModel {
    OpenAiResponsesModel::new(OpenAiResponsesConfig::new(
        responses_base(),
        "test-key",
        model,
    ))
}

/// 从 bridge 转换后的 `ModelRequest` 中取出 assistant 消息携带的原生记录。
fn native_history_of(request: &ModelRequest) -> &ResponsesHistoryV1 {
    request
        .messages
        .iter()
        .find_map(|message| match message {
            ModelMessage::Assistant { content, .. } => {
                content.iter().find_map(|block| match block {
                    peri_model::ContentBlock::ResponsesNativeHistory { history } => {
                        Some(history.as_ref())
                    }
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("assistant message carries responses native history")
}

/// 记录 → 真实 SQLite → 加载后的消息列表（含 AI 消息与工具结果）。
async fn persisted_messages() -> (tempfile::TempDir, Vec<BaseMessage>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("responses-lifecycle.db"))
            .await
            .expect("sqlite store"),
    );
    let thread_id = store
        .create_thread(ThreadMeta::new("/tmp"))
        .await
        .expect("thread");

    let bridge = AgentModelBridge::new(Arc::new(ResponsesResponseModel));
    let reasoning = bridge
        .generate_reasoning(&[BaseMessage::human("调用工具看看")], &[], None)
        .await
        .expect("responses reasoning");

    let source_message = reasoning
        .source_message
        .clone()
        .expect("reasoning carries source message");
    store
        .append_message(&thread_id, source_message)
        .await
        .expect("persist source message");
    store
        .append_message(&thread_id, BaseMessage::tool_result("call_1", "总用量 12"))
        .await
        .expect("persist tool result");

    let loaded = store.load_messages(&thread_id).await.expect("load history");
    (dir, loaded)
}

/// 整批消息经真实 bridge + adapter 的观测请求投影（LlmRequestPayload 同源）。
fn observed_requests_body(loaded: &[BaseMessage], model: &str) -> serde_json::Value {
    let bridge = AgentModelBridge::new(Arc::new(responses_adapter(model)));
    bridge
        .observed_provider_request_body(loaded, &[])
        .expect("responses observed body")
}

#[tokio::test]
async fn responses_reasoning_survives_sqlite_and_keeps_usage_and_request_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("responses-usage.db"))
            .await
            .expect("sqlite store"),
    );
    let thread_id = store
        .create_thread(ThreadMeta::new("/tmp"))
        .await
        .expect("thread");

    let bridge = AgentModelBridge::new(Arc::new(ResponsesResponseModel));
    let reasoning = bridge
        .generate_reasoning(&[BaseMessage::human("调用工具看看")], &[], None)
        .await
        .expect("responses reasoning");

    let usage = reasoning.usage.as_ref().expect("usage preserved");
    assert_eq!(usage.input_tokens, 123);
    assert_eq!(usage.output_tokens, 45);
    assert_eq!(usage.cache_read_input_tokens, Some(7));
    assert_eq!(reasoning.request_id.as_deref(), Some("req_responses_1"));
    assert_eq!(reasoning.stop_reason, StopReason::ToolUse);

    let source_message = reasoning
        .source_message
        .clone()
        .expect("reasoning carries source message");
    assert!(
        source_message.has_provider_native_history(),
        "Responses 响应必须把原生记录带进 Agent 消息"
    );
    let calls = source_message.tool_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].name, "shell");
    assert_eq!(calls[0].arguments, json!({"command": "ls -la"}));

    // 真实 SQLite 保存/加载后记录仍逐字可解析
    store
        .append_message(&thread_id, source_message)
        .await
        .expect("persist");
    let loaded = store.load_messages(&thread_id).await.expect("load");
    assert_eq!(loaded.len(), 1);
    let converted = AgentModelBridge::convert_message(&loaded[0]).expect("assistant converts");
    let request = ModelRequest::new(vec![converted]);
    let history = native_history_of(&request);
    assert_eq!(history.visible_text(), "答案正文");
    assert_eq!(history.tool_calls().len(), 1);
}

#[tokio::test]
async fn responses_history_replays_for_same_source_without_duplicates() {
    let (_dir, loaded) = persisted_messages().await;
    assert_eq!(loaded.len(), 2, "SQLite 必须保存 AI 消息与工具结果");

    let assistant = loaded
        .iter()
        .find(|message| matches!(message, BaseMessage::Ai { .. }))
        .expect("assistant message in history");
    let converted = AgentModelBridge::convert_message(assistant).expect("assistant converts");
    let request = ModelRequest::new(vec![converted]);
    let history = native_history_of(&request);

    // 同源：记录契约判定可回放
    assert_eq!(
        history.verify_source(&responses_endpoint(), RESPONSES_MODEL),
        Ok(())
    );
    let items = history
        .project_input_items(&responses_endpoint(), RESPONSES_MODEL)
        .expect("same-source replay");

    let reasoning_items: Vec<_> = items
        .iter()
        .filter(|item| item.as_map().get("type") == Some(&json!("reasoning")))
        .collect();
    assert_eq!(reasoning_items.len(), 1, "reasoning 记录必须恰好回放一次");
    assert_eq!(
        reasoning_items[0].as_map().get("encrypted_content"),
        Some(&json!(ENCRYPTED_REASONING)),
        "同源回放必须携带 reasoning 密文"
    );

    let messages: Vec<_> = items
        .iter()
        .filter(|item| item.as_map().get("type") == Some(&json!("message")))
        .collect();
    assert_eq!(messages.len(), 1, "可见文本必须恰好回放一次");

    let calls: Vec<_> = items
        .iter()
        .filter(|item| item.as_map().get("type") == Some(&json!("function_call")))
        .collect();
    assert_eq!(calls.len(), 1, "工具调用不得重复回放");
    assert_eq!(calls[0].as_map().get("call_id"), Some(&json!("call_1")));
    assert_eq!(
        calls[0].as_map().get("arguments"),
        Some(&json!(TOOL_ARGUMENTS)),
        "工具参数必须字节保真"
    );

    // 工具结果与调用通过 call_id 配对，且只出现一次（整批消息经真实 bridge + adapter）
    let observed = observed_requests_body(&loaded, RESPONSES_MODEL);
    let observed_input = observed["input"].as_array().expect("observed input items");
    let outputs: Vec<_> = observed_input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect();
    assert_eq!(outputs.len(), 1, "工具结果不得重复");
    assert_eq!(outputs[0]["call_id"], "call_1");
    let observed_calls: Vec<_> = observed_input
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect();
    assert_eq!(observed_calls.len(), 1, "工具调用不得重复");
    assert_eq!(observed_calls[0]["call_id"], "call_1");

    // 回放顺序与原生记录一致
    let types: Vec<&str> = items
        .iter()
        .filter_map(|item| item.as_map().get("type").and_then(|value| value.as_str()))
        .collect();
    assert_eq!(types, vec!["reasoning", "message", "function_call"]);
}

#[test]
fn responses_history_degrades_without_ciphertext_across_sources() {
    let history = sample_history();
    let other_model = "gpt-4.1";

    assert_eq!(
        history.verify_source(&responses_endpoint(), other_model),
        Err(peri_model::HistoryError::SourceMismatch),
        "不同 model 必须被判定为跨来源"
    );

    // adapter 在 SourceMismatch 时使用的降级投影：无 reasoning、无密文
    let items = history.project_generic_input_items();
    let serialized = serde_json::to_string(&items).expect("serialize");
    assert!(!serialized.contains(ENCRYPTED_REASONING));
    assert!(items
        .iter()
        .all(|item| item.as_map().get("type") != Some(&json!("reasoning"))));

    let calls: Vec<_> = items
        .iter()
        .filter(|item| item.as_map().get("type") == Some(&json!("function_call")))
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].as_map().get("call_id"), Some(&json!("call_1")));
    assert_eq!(
        calls[0].as_map().get("arguments"),
        Some(&json!(TOOL_ARGUMENTS))
    );
    let messages: Vec<_> = items
        .iter()
        .filter(|item| item.as_map().get("type") == Some(&json!("message")))
        .collect();
    assert_eq!(messages.len(), 1, "降级必须保留可见文本");
}

#[test]
fn corrupt_native_history_is_rejected_instead_of_dropped() {
    // 版本 2 不在支持范围：必须显式失败，不能静默丢弃记录。
    let unsupported = ContentBlock::Unknown(json!({
        "type": crate::messages::RESPONSES_NATIVE_HISTORY_TAG,
        "history": {"version": 2, "source": {"nonce": "00", "digest": "00"}, "items": []},
    }));
    let message = BaseMessage::ai(MessageContent::Blocks(vec![unsupported]));
    assert!(
        AgentModelBridge::convert_message(&message).is_err(),
        "未知版本必须拒绝"
    );

    // 缺少 history 载荷同样拒绝。
    let missing = ContentBlock::Unknown(json!({
        "type": crate::messages::RESPONSES_NATIVE_HISTORY_TAG,
    }));
    let message = BaseMessage::ai(MessageContent::Blocks(vec![missing]));
    assert!(
        AgentModelBridge::convert_message(&message).is_err(),
        "缺失载荷必须拒绝"
    );

    // 结构损坏（function_call 缺 arguments/status）同样拒绝。
    let corrupted = ContentBlock::responses_native_history(json!({
        "version": 1,
        "source": {"nonce": "00".repeat(16), "digest": "00".repeat(32)},
        "items": [{"type": "function_call", "id": "fc", "call_id": "c", "name": "shell"}],
    }));
    let message = BaseMessage::ai(MessageContent::Blocks(vec![corrupted]));
    assert!(
        AgentModelBridge::convert_message(&message).is_err(),
        "损坏记录必须拒绝"
    );
}

#[tokio::test]
async fn responses_history_roundtrips_through_bridge_faithfully() {
    let bridge = AgentModelBridge::new(Arc::new(ResponsesResponseModel));
    let reasoning = bridge
        .generate_reasoning(&[BaseMessage::human("调用工具看看")], &[], None)
        .await
        .expect("responses reasoning");
    let source_message = reasoning
        .source_message
        .clone()
        .expect("reasoning carries source message");

    // 持久化载体保真：Agent 侧必须原样保存记录（脱敏只作用于可观测出口）
    let wire = serde_json::to_string(&source_message).expect("agent content serializes");
    assert!(wire.contains(ENCRYPTED_REASONING));
    assert!(wire.contains("ls -la"));

    // Agent → Model（下一轮请求输入）：记录等价，派生视图与工具调用不变
    let roundtrip = AgentModelBridge::convert_message(&source_message).expect("content converts");
    match roundtrip {
        ModelMessage::Assistant {
            content,
            tool_calls,
        } => {
            let restored = content
                .iter()
                .find_map(|block| match block {
                    peri_model::ContentBlock::ResponsesNativeHistory { history } => {
                        Some(history.as_ref())
                    }
                    _ => None,
                })
                .expect("native history survives roundtrip");
            assert_eq!(restored.visible_text(), "答案正文");
            assert_eq!(restored.tool_calls(), tool_calls);
            let restored_wire = serde_json::to_string(restored).expect("history serializes");
            assert!(
                restored_wire.contains(ENCRYPTED_REASONING),
                "记录密文不得丢失"
            );
            assert!(restored_wire.contains("ls -la"), "记录参数不得丢失");
        }
        other => panic!("assistant message expected, got {other:?}"),
    }
}

#[test]
fn observability_exit_redacts_private_state() {
    let history = sample_history();
    let message = BaseMessage::ai(MessageContent::Blocks(vec![
        ContentBlock::text("答案正文"),
        ContentBlock::responses_native_history(
            serde_json::to_value(&history).expect("serialize history"),
        ),
    ]));

    // 事件快照（LlmCallStart / MessagesCompacted 等观察出口）
    let observed =
        crate::agent::stages::observed_message_snapshot(&Arc::new(vec![message.clone()]));
    let wire = serde_json::to_string(&observed).expect("serialize observed messages");
    assert!(!wire.contains(ENCRYPTED_REASONING), "事件快照不得携带密文");
    assert!(wire.contains("答案正文"), "可见文本保留");

    // 请求观测出口（LlmRequestPayload）：peri-model 的观测投影始终脱敏敏感键
    let tool_calls = history.tool_calls();
    let request = ModelRequest::new(vec![ModelMessage::assistant(
        vec![
            peri_model::ContentBlock::Reasoning {
                text: history.reasoning_summary_text(),
                signature: None,
            },
            peri_model::ContentBlock::text(history.visible_text()),
            peri_model::ContentBlock::ResponsesNativeHistory {
                history: Box::new(history),
            },
        ],
        tool_calls,
    )]);
    let prepared = responses_adapter(RESPONSES_MODEL)
        .prepare_request(&request)
        .expect("observed body");
    let observed_body = serde_json::to_string(prepared.body().as_value()).expect("body json");
    assert!(
        !observed_body.contains(ENCRYPTED_REASONING),
        "观测体不得携带密文"
    );
    assert!(
        !observed_body.contains("ls -la"),
        "观测体不得携带编码工具参数"
    );
    assert!(
        prepared
            .redacted_paths()
            .iter()
            .any(|path| path.contains("encrypted_content")),
        "脱敏路径必须记录 encrypted_content：{:?}",
        prepared.redacted_paths()
    );

    // Debug 出口（摘要/日志）
    let debug = format!("{message:?}");
    assert!(!debug.contains(ENCRYPTED_REASONING));
}

#[test]
fn history_source_identity_is_not_debug_visible() {
    let history = sample_history();
    let debug = format!("{history:?}");
    assert!(!debug.contains("nonce"));
    assert!(!debug.contains("digest"));
    assert!(debug.contains("item_count"), "记录仍应暴露规模供诊断");
}
