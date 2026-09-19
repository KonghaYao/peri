use std::collections::BTreeMap;

use serde_json::{json, Value};
use url::Url;

use crate::{
    prompt_cache::strip_system_prompt_dynamic_boundaries, ContentBlock, DocumentSource,
    HistoryError, ImageSource, JsonObject, ModelError, ModelMessage, ModelRequest, ModelResult,
    PreparedModelRequest, ProviderProtocol, ResponsesHistoryV1, ToolCall,
};

use super::OpenAiResponsesConfig;

/// 无状态续轮所需的 include 值：官方在 `store: false` 下用它返回 reasoning 密文。
const INCLUDE_ENCRYPTED_REASONING: &str = "reasoning.encrypted_content";

#[derive(Clone)]
pub(super) struct BuiltResponsesRequest {
    pub(super) endpoint: Url,
    pub(super) model_id: String,
    pub(super) body: Value,
    diagnostics: BTreeMap<String, Value>,
}

impl BuiltResponsesRequest {
    pub(super) fn observe(
        &self,
        runtime: &crate::ModelRuntimeConfig,
    ) -> ModelResult<PreparedModelRequest> {
        PreparedModelRequest::observe_with_runtime(
            ProviderProtocol::OpenAiResponses,
            self.model_id.clone(),
            self.endpoint.clone(),
            self.body.clone(),
            self.diagnostics.clone(),
            runtime,
        )
    }
}

pub(super) fn build_request(
    config: &OpenAiResponsesConfig,
    request: &ModelRequest,
) -> ModelResult<BuiltResponsesRequest> {
    let endpoint = responses_endpoint(&config.endpoint)?;
    let mut body = json!({
        "model": config.model,
        "input": input_items(&request.messages, &endpoint, &config.model)?,
        "stream": true,
        // 默认无状态：不把响应留给服务端存储，续轮完全依赖本地原生记录。
        "store": false,
        "include": [INCLUDE_ENCRYPTED_REASONING],
        "max_output_tokens": request.max_tokens.unwrap_or(config.max_output_tokens),
    });

    if let Some(instructions) = system_instructions(&request.messages) {
        body["instructions"] = json!(instructions);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(tool_to_responses).collect());
        body["tool_choice"] = json!("auto");
    }
    if let Some(effort) = &config.reasoning_effort {
        body["reasoning"] = json!({ "effort": effort });
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(session_id) = &request.session_id {
        body["metadata"] = json!({ "session_id": session_id });
    }

    Ok(BuiltResponsesRequest {
        diagnostics: PreparedModelRequest::history_diagnostics(
            request,
            ProviderProtocol::OpenAiResponses,
            &endpoint,
            &config.model,
        ),
        endpoint,
        model_id: config.model.clone(),
        body,
    })
}

/// 真实请求 endpoint：`{base}/responses`。来源身份绑定的是这里的结果。
pub(super) fn responses_endpoint(endpoint: &Url) -> ModelResult<Url> {
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
    {
        return Err(ModelError::protocol(
            crate::ProtocolErrorKind::InvalidEndpoint,
        ));
    }

    let mut endpoint = endpoint.clone();
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    let mut path_segments = endpoint
        .path_segments_mut()
        .map_err(|_| ModelError::protocol(crate::ProtocolErrorKind::InvalidEndpoint))?;
    path_segments.pop_if_empty();
    path_segments.push("responses");
    drop(path_segments);
    Ok(endpoint)
}

fn system_instructions(messages: &[ModelMessage]) -> Option<String> {
    let parts = messages
        .iter()
        .filter_map(|message| match message {
            ModelMessage::System { content } => Some(text_only(content)),
            _ => None,
        })
        .filter(|content| !content.trim().is_empty())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| strip_system_prompt_dynamic_boundaries(&parts.join("\n\n")))
}

fn text_only(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(ContentBlock::text_content)
        .collect()
}

fn input_items(messages: &[ModelMessage], endpoint: &Url, model: &str) -> ModelResult<Vec<Value>> {
    let mut items = Vec::new();
    for message in messages {
        match message {
            // system 走顶层 `instructions`，不重复进入 input。
            ModelMessage::System { .. } => {}
            ModelMessage::User { content } => items.push(json!({
                "role": "user",
                "content": input_content(content)?,
            })),
            ModelMessage::Assistant {
                content,
                tool_calls,
            } => items.extend(assistant_items(content, tool_calls, endpoint, model)?),
            ModelMessage::ToolResult { result } => items.push(json!({
                "type": "function_call_output",
                "call_id": result.tool_call_id,
                // 官方 function_call_output 无错误标志位，失败语义由输出文本承载。
                "output": tool_output(result)?,
            })),
        }
    }
    Ok(items)
}

fn tool_output(result: &crate::ToolResult) -> ModelResult<Value> {
    let output = input_content(&result.content)?;
    if !result.is_error {
        return Ok(output);
    }
    // Responses 没有 is_error 字段，显式传达执行失败，保留原始多模态结果。
    match output {
        Value::String(text) => Ok(json!(format!("Tool execution failed:\n{text}"))),
        Value::Array(mut parts) => {
            parts.insert(
                0,
                json!({"type": "input_text", "text": "Tool execution failed:"}),
            );
            Ok(Value::Array(parts))
        }
        _ => Err(unsupported_semantics()),
    }
}

fn assistant_items(
    content: &[ContentBlock],
    tool_calls: &[ToolCall],
    endpoint: &Url,
    model: &str,
) -> ModelResult<Vec<Value>> {
    let mut histories = content.iter().filter_map(|block| match block {
        ContentBlock::ResponsesNativeHistory { history } => Some(history.as_ref()),
        _ => None,
    });
    let Some(history) = histories.next() else {
        return derived_assistant_items(content, tool_calls);
    };
    if histories.next().is_some() {
        return Err(unsupported_semantics());
    }
    replay_native_history(history, content, tool_calls, endpoint, model)
}

/// 原生记录回放：记录是唯一事实，同消息的文本/推理/工具调用是它的派生视图。
///
/// 同 endpoint/model 域内按记录逐项回放（含 reasoning 密文）；来源不匹配时降级为
/// 通用 message/function_call，密文绝不跨来源回放。
fn replay_native_history(
    history: &ResponsesHistoryV1,
    content: &[ContentBlock],
    tool_calls: &[ToolCall],
    endpoint: &Url,
    model: &str,
) -> ModelResult<Vec<Value>> {
    for block in content {
        match block {
            ContentBlock::Text { .. }
            | ContentBlock::Reasoning { .. }
            | ContentBlock::RedactedReasoning { .. }
            | ContentBlock::ResponsesNativeHistory { .. } => {}
            ContentBlock::Image { .. }
            | ContentBlock::Document { .. }
            | ContentBlock::ToolUse { .. }
            | ContentBlock::ToolResult { .. } => return Err(unsupported_semantics()),
        }
    }
    verify_derived_views(history, content, tool_calls)?;

    let items = match history.project_input_items(endpoint, model) {
        Ok(items) => items,
        Err(HistoryError::SourceMismatch) => history.project_generic_input_items(),
        Err(_) => return Err(unsupported_semantics()),
    };
    Ok(items.into_iter().map(json_object).collect())
}

/// 派生视图必须与原生记录一致。
///
/// 记录是唯一回放来源，派生文本/工具调用只用于本地消费；两者不一致说明内容被外部
/// 改写（例如 compact 拆开了原生单元），此时静默以记录为准会丢掉改写后的内容，因此
/// 显式失败。
fn verify_derived_views(
    history: &ResponsesHistoryV1,
    content: &[ContentBlock],
    tool_calls: &[ToolCall],
) -> ModelResult<()> {
    let mut expected = history.visible_text();
    for refusal in history.refusals() {
        expected.push_str(refusal);
    }
    let mut actual = String::new();
    for block in content {
        if let ContentBlock::Text { text } = block {
            actual.push_str(text);
        }
    }
    if actual != expected {
        return Err(unsupported_semantics());
    }

    let recorded = history.tool_calls();
    if recorded.len() != tool_calls.len()
        || recorded.iter().zip(tool_calls).any(|(recorded, derived)| {
            recorded.id() != derived.id()
                || recorded.name() != derived.name()
                || recorded.arguments() != derived.arguments()
        })
    {
        return Err(unsupported_semantics());
    }
    Ok(())
}

/// 无原生记录的 assistant 消息：只回放可见推理文本与工具调用。
fn derived_assistant_items(
    content: &[ContentBlock],
    tool_calls: &[ToolCall],
) -> ModelResult<Vec<Value>> {
    let mut items = Vec::new();
    let mut text_parts = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text { text } => text_parts.push(json!({
                "type": "input_text",
                "text": text,
            })),
            ContentBlock::Reasoning { text, .. } if !text.is_empty() => {
                items.push(reasoning_item(text));
            }
            // 跨协议不可回放：只承载 provider 私有推理状态的 block 不翻译成可见文本。
            ContentBlock::Reasoning { .. } | ContentBlock::RedactedReasoning { .. } => {}
            ContentBlock::Image { .. }
            | ContentBlock::Document { .. }
            | ContentBlock::ToolUse { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::ResponsesNativeHistory { .. } => return Err(unsupported_semantics()),
        }
    }
    if !text_parts.is_empty() {
        items.push(json!({ "role": "assistant", "content": text_parts }));
    }
    items.extend(tool_calls.iter().map(|tool_call| {
        json!({
            "type": "function_call",
            "call_id": tool_call.id(),
            "name": tool_call.name(),
            "arguments": arguments_to_wire(tool_call),
        })
    }));
    Ok(items)
}

/// 官方 reasoning item 的 input 形状；与原生记录投影使用同一白名单字段。
fn reasoning_item(text: &str) -> Value {
    json!({
        "type": "reasoning",
        "summary": [{ "type": "summary_text", "text": text }],
    })
}

fn arguments_to_wire(tool_call: &ToolCall) -> String {
    serde_json::to_string(tool_call.arguments().as_map()).expect("JsonObject always serializes")
}

fn json_object(object: JsonObject) -> Value {
    Value::Object(object.as_map().clone().into_iter().collect())
}

/// user / function_call_output 的内容：单一文本退化为字符串，其余为官方 content 数组。
fn input_content(content: &[ContentBlock]) -> ModelResult<Value> {
    if let [ContentBlock::Text { text }] = content {
        return Ok(json!(text));
    }
    let parts = content
        .iter()
        .map(input_part)
        .collect::<ModelResult<Vec<_>>>()?;
    Ok(match parts.as_slice() {
        [] => json!(""),
        [Value::String(text)] => json!(text),
        _ => Value::Array(parts),
    })
}

fn input_part(block: &ContentBlock) -> ModelResult<Value> {
    match block {
        ContentBlock::Text { text } => Ok(json!({ "type": "input_text", "text": text })),
        ContentBlock::Image { source } => Ok(json!({
            "type": "input_image",
            "image_url": image_url(source),
        })),
        ContentBlock::Document { source, title } => match source {
            DocumentSource::Text { text } => Ok(json!({ "type": "input_text", "text": text })),
            DocumentSource::Url { url } => Ok(json!({ "type": "input_file", "file_url": url })),
            // 官方文件输入示例使用 data URI；这里只支持已核实的 PDF 编码。
            DocumentSource::Base64 { media_type, data }
                if media_type.as_str() == "application/pdf" =>
            {
                Ok(json!({
                    "type": "input_file",
                    "filename": title.as_deref().filter(|title| title.ends_with(".pdf")).unwrap_or("document.pdf"),
                    "file_data": format!("data:application/pdf;base64,{data}"),
                }))
            }
            DocumentSource::Base64 { .. } => Err(unsupported_semantics()),
        },
        ContentBlock::Reasoning { .. }
        | ContentBlock::ToolUse { .. }
        | ContentBlock::ToolResult { .. }
        | ContentBlock::RedactedReasoning { .. }
        | ContentBlock::ResponsesNativeHistory { .. } => Err(unsupported_semantics()),
    }
}

fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Url { url } => url.clone(),
        ImageSource::Base64 { media_type, data } => {
            format!("data:{};base64,{data}", media_type.as_str())
        }
    }
}

fn tool_to_responses(tool: &crate::ToolDefinition) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema.as_map(),
        // 官方省略 strict 时会尝试把 schema 规范化到 strict；显式关闭以保持项目原 schema。
        "strict": false,
    })
}

fn unsupported_semantics() -> ModelError {
    ModelError::protocol(crate::ProtocolErrorKind::Other)
}

#[cfg(test)]
pub(super) fn body_for_test(config: &OpenAiResponsesConfig, request: &ModelRequest) -> Value {
    build_request(config, request)
        .expect("test config is valid")
        .body
}
