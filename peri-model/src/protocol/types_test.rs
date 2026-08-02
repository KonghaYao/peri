use serde_json::{json, Value};

use crate::ProtocolErrorKind;

use super::{
    ContentBlock, DocumentSource, ImageSource, JsonObject, MediaType, ModelCapabilities,
    ModelMessage, ModelRequest, ModelResponse, ProviderProtocol, StopReason, TokenUsage, ToolCall,
    ToolDefinition, ToolResult,
};

fn json_object(value: Value) -> JsonObject {
    JsonObject::from_value(value).unwrap()
}

fn tool_call() -> ToolCall {
    ToolCall::new("call_1", "shell", json_object(json!({"command": "pwd"})))
}

fn tool_result(is_error: bool) -> ToolResult {
    ToolResult {
        id: Some("provider_result_1".into()),
        tool_call_id: "call_1".into(),
        name: "shell".into(),
        content: vec![ContentBlock::text(if is_error {
            "command failed"
        } else {
            "/workspace"
        })],
        is_error,
    }
}

#[test]
fn test_tool_call_new_preserves_id_name_and_arguments() {
    let call = ToolCall::new("call_2", "read", json_object(json!({"path": "Cargo.toml"})));

    assert_eq!(call.id(), "call_2");
    assert_eq!(call.name(), "read");
    assert_eq!(
        call.arguments().as_map().get("path"),
        Some(&json!("Cargo.toml"))
    );
}

#[test]
fn test_tool_result_success_and_error_preserve_fields_and_status() {
    let success = ToolResult::success("call_2", "read", "contents");
    let error = ToolResult::error("call_3", "write", "permission denied");

    assert_eq!(success.id, None);
    assert_eq!(success.tool_call_id, "call_2");
    assert_eq!(success.name, "read");
    assert_eq!(success.content, vec![ContentBlock::text("contents")]);
    assert!(success.is_success());
    assert!(!success.is_error);

    assert_eq!(error.id, None);
    assert_eq!(error.tool_call_id, "call_3");
    assert_eq!(error.name, "write");
    assert_eq!(error.content, vec![ContentBlock::text("permission denied")]);
    assert!(!error.is_success());
    assert!(error.is_error);
}

#[test]
fn test_content_blocks_serde_roundtrip() {
    let tool_call = tool_call();
    let blocks = vec![
        ("text", ContentBlock::text("hello")),
        (
            "image_base64",
            ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: MediaType::new("image/png"),
                    data: "iVBORw0KGgo=".into(),
                },
            },
        ),
        (
            "image_url",
            ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.test/image.png".into(),
                },
            },
        ),
        (
            "document_base64",
            ContentBlock::Document {
                source: DocumentSource::Base64 {
                    media_type: MediaType::new("application/pdf"),
                    data: "JVBERi0=".into(),
                },
                title: Some("设计文档".into()),
            },
        ),
        (
            "document_url",
            ContentBlock::Document {
                source: DocumentSource::Url {
                    url: "https://example.test/design.pdf".into(),
                },
                title: None,
            },
        ),
        (
            "document_text",
            ContentBlock::Document {
                source: DocumentSource::Text {
                    text: "文档正文".into(),
                },
                title: Some("说明".into()),
            },
        ),
        (
            "reasoning",
            ContentBlock::Reasoning {
                text: "需要运行 shell".into(),
                signature: Some("provider-signature".into()),
            },
        ),
        (
            "tool_use",
            ContentBlock::ToolUse {
                tool_call: tool_call.clone(),
            },
        ),
        (
            "tool_result",
            ContentBlock::ToolResult {
                result: Box::new(tool_result(true)),
            },
        ),
        (
            "redacted_reasoning",
            ContentBlock::RedactedReasoning {
                data: Some("opaque-provider-data".into()),
            },
        ),
    ];
    for (name, block) in blocks {
        let encoded = serde_json::to_string(&block).unwrap();
        let decoded: ContentBlock = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, block, "{name} 内容块 roundtrip 应保留全部字段");
    }
}

#[test]
fn test_model_messages_serde_roundtrip_for_all_roles() {
    let messages = vec![
        ModelMessage::System {
            content: vec![ContentBlock::text("你是一个助手")],
        },
        ModelMessage::User {
            content: vec![ContentBlock::text("运行 pwd")],
        },
        ModelMessage::Assistant {
            content: vec![ContentBlock::reasoning("需要调用 shell")],
            tool_calls: vec![tool_call()],
        },
        ModelMessage::ToolResult {
            result: tool_result(false),
        },
    ];
    for message in messages {
        let encoded = serde_json::to_string(&message).unwrap();
        let decoded: ModelMessage = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, message, "所有 ModelMessage role 都应 roundtrip");
    }
}

#[test]
fn test_tool_results_serde_roundtrip_for_success_and_error() {
    let results = vec![
        ("success", tool_result(false)),
        ("error", tool_result(true)),
    ];
    for (name, result) in results {
        let encoded = serde_json::to_string(&result).unwrap();
        let decoded: ToolResult = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, result, "{name} ToolResult 应保留状态和内容");
        assert_eq!(decoded.is_success(), !decoded.is_error);
    }
}

#[test]
fn test_model_request_serde_roundtrip_preserves_tool_definition_and_tool_call_fields() {
    let tool_definition = ToolDefinition::new(
        "shell",
        json_object(json!({"type": "object", "properties": {"command": {"type": "string"}}})),
    )
    .with_description("运行 shell 命令");
    let request = ModelRequest::new(vec![ModelMessage::assistant(
        vec![ContentBlock::text("调用工具")],
        vec![tool_call()],
    )])
    .with_tools(vec![tool_definition.clone()])
    .with_max_tokens(512);
    let encoded = serde_json::to_string(&request).unwrap();
    let decoded: ModelRequest = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, request);
    assert_eq!(decoded.tools, vec![tool_definition]);
    let ModelMessage::Assistant { tool_calls, .. } = &decoded.messages[0] else {
        panic!("请求应保留 assistant 消息");
    };
    assert_eq!(tool_calls[0].id(), "call_1");
    assert_eq!(tool_calls[0].name(), "shell");
    assert_eq!(
        tool_calls[0].arguments().as_map().get("command"),
        Some(&json!("pwd"))
    );
}

#[test]
fn test_response_and_protocol_metadata_serde_roundtrip() {
    let usage = TokenUsage {
        input_tokens: 12,
        output_tokens: 34,
        cache_creation_input_tokens: Some(5),
        cache_read_input_tokens: Some(7),
    };
    let response = ModelResponse::new(
        ModelMessage::assistant_text("完成"),
        StopReason::EndTurn,
        Some(usage.clone()),
        Some("request_1".into()),
    )
    .unwrap();
    let capabilities = ModelCapabilities {
        supports_tools: true,
        supports_reasoning: true,
        supports_vision: true,
        supports_streaming: true,
    };
    let response_json = serde_json::to_string(&response).unwrap();
    assert_eq!(
        serde_json::from_str::<ModelResponse>(&response_json).unwrap(),
        response
    );
    assert_eq!(
        serde_json::from_str::<TokenUsage>(&serde_json::to_string(&usage).unwrap()).unwrap(),
        usage
    );
    assert_eq!(
        serde_json::from_str::<ModelCapabilities>(&serde_json::to_string(&capabilities).unwrap())
            .unwrap(),
        capabilities
    );
}

#[test]
fn test_stop_reasons_serde_roundtrip_for_all_variants() {
    let reasons = vec![
        StopReason::EndTurn,
        StopReason::ToolUse,
        StopReason::MaxTokens,
        StopReason::Other {
            value: "provider_specific".into(),
        },
    ];
    for reason in reasons {
        let encoded = serde_json::to_string(&reason).unwrap();
        let decoded: StopReason = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, reason, "所有 StopReason 变体都应 roundtrip");
    }
}

#[test]
fn test_provider_protocols_serde_roundtrip_for_all_variants() {
    let protocols = vec![
        ProviderProtocol::OpenAiCompatible,
        ProviderProtocol::Anthropic,
        ProviderProtocol::Other {
            value: "custom".into(),
        },
    ];
    for protocol in protocols {
        let encoded = serde_json::to_string(&protocol).unwrap();
        let decoded: ProviderProtocol = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            decoded, protocol,
            "所有 ProviderProtocol 变体都应 roundtrip"
        );
    }
}

#[test]
fn test_model_response_requires_an_assistant_message() {
    let error = ModelResponse::new(
        ModelMessage::user_text("x"),
        StopReason::EndTurn,
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(ProtocolErrorKind::AssistantMessageRequired)
    );
}

#[test]
fn test_model_response_deserialization_requires_an_assistant_message() {
    let error = serde_json::from_value::<ModelResponse>(json!({
        "message": {"role": "user", "content": [{"type": "text", "text": "x"}]},
        "stop_reason": {"kind": "end_turn"}
    }))
    .unwrap_err();
    assert!(error.to_string().contains("assistant message"));
}

#[test]
fn test_json_object_rejects_non_object_values() {
    let error = JsonObject::from_value(json!(["not", "an", "object"])).unwrap_err();
    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(ProtocolErrorKind::InvalidJsonObject)
    );
}
