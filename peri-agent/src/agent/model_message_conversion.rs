//! Canonical messages converted to provider message blocks.

use crate::error::AgentError;
use crate::error::AgentResult;
use crate::messages::BaseMessage;
use crate::messages::ContentBlock;
use crate::messages::DocumentSource;
use crate::messages::ImageSource;
use crate::messages::MessageContent;

use crate::messages::ToolCallRequest;

use peri_model::ContentBlock as ModelContentBlock;
use peri_model::DocumentSource as ModelDocumentSource;
use peri_model::ImageSource as ModelImageSource;
use peri_model::JsonObject;

use peri_model::ModelMessage;

use peri_model::ToolCall as ModelToolCall;

use peri_model::ToolResult as ModelToolResult;

pub(super) fn convert_tool_call(call: &ToolCallRequest) -> AgentResult<ModelToolCall> {
    Ok(ModelToolCall::new(
        call.id.clone(),
        call.name.clone(),
        JsonObject::from_value(call.arguments.clone()).map_err(map_model_error)?,
    ))
}

pub(super) fn convert_content(content: &MessageContent) -> AgentResult<Vec<ModelContentBlock>> {
    match content {
        MessageContent::Text(text) => Ok(vec![ModelContentBlock::text(text)]),
        MessageContent::Raw(_) => Err(AgentError::LlmError(
            "unsupported agent content: raw provider content".into(),
        )),
        MessageContent::Blocks(blocks) => blocks.iter().map(convert_block).collect(),
    }
}

pub(super) fn convert_block(block: &ContentBlock) -> AgentResult<ModelContentBlock> {
    match block {
        ContentBlock::Text { text } => Ok(ModelContentBlock::text(text)),
        ContentBlock::Image { source } => Ok(ModelContentBlock::Image {
            source: match source {
                ImageSource::Base64 { media_type, data } => ModelImageSource::Base64 {
                    media_type: peri_model::MediaType::new(media_type),
                    data: data.clone(),
                },
                ImageSource::Url { url } => ModelImageSource::Url { url: url.clone() },
            },
        }),
        ContentBlock::Document { source, title } => Ok(ModelContentBlock::Document {
            source: match source {
                DocumentSource::Base64 { media_type, data } => ModelDocumentSource::Base64 {
                    media_type: peri_model::MediaType::new(media_type),
                    data: data.clone(),
                },
                DocumentSource::Url { url } => ModelDocumentSource::Url { url: url.clone() },
                DocumentSource::Text { text } => ModelDocumentSource::Text { text: text.clone() },
            },
            title: title.clone(),
        }),
        ContentBlock::ToolUse { id, name, input } => Ok(ModelContentBlock::ToolUse {
            tool_call: ModelToolCall::new(
                id.clone(),
                name.clone(),
                JsonObject::from_value(input.clone()).map_err(map_model_error)?,
            ),
        }),
        ContentBlock::ToolResult {
            id,
            tool_use_id,
            content,
            is_error,
        } => Ok(ModelContentBlock::ToolResult {
            result: Box::new(ModelToolResult {
                id: id.clone(),
                tool_call_id: tool_use_id.clone(),
                name: "unknown".into(),
                content: content
                    .iter()
                    .map(convert_block)
                    .collect::<AgentResult<_>>()?,
                is_error: *is_error,
            }),
        }),
        ContentBlock::Reasoning { text, signature } => Ok(ModelContentBlock::Reasoning {
            text: text.clone(),
            signature: signature.clone(),
        }),
        ContentBlock::ProviderState { provider, data } => Ok(ModelContentBlock::ProviderState {
            provider: provider.clone(),
            data: data.clone(),
        }),
        ContentBlock::Unknown(value) => {
            // Agent 暂无显式 RedactedReasoning 变体；标准载荷经 Unknown 容器往返。
            // 仅识别 peri_model::RedactedReasoning 的确定形状，其余 Unknown 一律 fail closed。
            if value.get("type").and_then(|t| t.as_str()) == Some("redacted_reasoning") {
                let data = value
                    .get("data")
                    .and_then(|d| d.as_str())
                    .map(str::to_owned);
                return Ok(ModelContentBlock::RedactedReasoning { data });
            }
            Err(AgentError::LlmError(
                "unsupported agent content: unknown content block".into(),
            ))
        }
    }
}

pub(super) fn convert_model_message(message: &ModelMessage) -> AgentResult<BaseMessage> {
    match message {
        ModelMessage::System { content } => Ok(BaseMessage::system(MessageContent::Blocks(
            content
                .iter()
                .map(convert_model_block)
                .collect::<AgentResult<_>>()?,
        ))),
        ModelMessage::User { content } => Ok(BaseMessage::human(MessageContent::Blocks(
            content
                .iter()
                .map(convert_model_block)
                .collect::<AgentResult<_>>()?,
        ))),
        ModelMessage::Assistant {
            content,
            tool_calls,
        } => Ok(BaseMessage::ai_with_tool_calls(
            MessageContent::Blocks(
                content
                    .iter()
                    .map(convert_model_block)
                    .collect::<AgentResult<_>>()?,
            ),
            tool_calls
                .iter()
                .map(|call| {
                    ToolCallRequest::new(
                        call.id(),
                        call.name(),
                        serde_json::Value::Object(
                            call.arguments().as_map().clone().into_iter().collect(),
                        ),
                    )
                })
                .collect(),
        )),
        ModelMessage::ToolResult { result } => Ok(if result.is_error {
            BaseMessage::tool_error(
                &result.tool_call_id,
                MessageContent::Blocks(
                    result
                        .content
                        .iter()
                        .map(convert_model_block)
                        .collect::<AgentResult<_>>()?,
                ),
            )
        } else {
            BaseMessage::tool_result(
                &result.tool_call_id,
                MessageContent::Blocks(
                    result
                        .content
                        .iter()
                        .map(convert_model_block)
                        .collect::<AgentResult<_>>()?,
                ),
            )
        }),
    }
}

pub(super) fn convert_model_block(block: &ModelContentBlock) -> AgentResult<ContentBlock> {
    match block {
        ModelContentBlock::Text { text } => Ok(ContentBlock::text(text)),
        ModelContentBlock::Image { source } => Ok(ContentBlock::Image {
            source: match source {
                ModelImageSource::Base64 { media_type, data } => ImageSource::Base64 {
                    media_type: media_type.as_str().into(),
                    data: data.clone(),
                },
                ModelImageSource::Url { url } => ImageSource::Url { url: url.clone() },
            },
        }),
        ModelContentBlock::Document { source, title } => Ok(ContentBlock::Document {
            source: match source {
                ModelDocumentSource::Base64 { media_type, data } => DocumentSource::Base64 {
                    media_type: media_type.as_str().into(),
                    data: data.clone(),
                },
                ModelDocumentSource::Url { url } => DocumentSource::Url { url: url.clone() },
                ModelDocumentSource::Text { text } => DocumentSource::Text { text: text.clone() },
            },
            title: title.clone(),
        }),
        ModelContentBlock::Reasoning { text, signature } => Ok(ContentBlock::Reasoning {
            text: text.clone(),
            signature: signature.clone(),
        }),
        ModelContentBlock::ProviderState { provider, data } => Ok(ContentBlock::ProviderState {
            provider: provider.clone(),
            data: data.clone(),
        }),
        ModelContentBlock::ToolUse { tool_call } => Ok(ContentBlock::ToolUse {
            id: tool_call.id().into(),
            name: tool_call.name().into(),
            input: serde_json::Value::Object(
                tool_call.arguments().as_map().clone().into_iter().collect(),
            ),
        }),
        ModelContentBlock::ToolResult { result } => Ok(ContentBlock::ToolResult {
            id: result.id.clone(),
            tool_use_id: result.tool_call_id.clone(),
            content: result
                .content
                .iter()
                .map(convert_model_block)
                .collect::<AgentResult<_>>()?,
            is_error: result.is_error,
        }),
        ModelContentBlock::RedactedReasoning { data } => {
            Ok(ContentBlock::Unknown(serde_json::json!({
                "type": "redacted_reasoning",
                "data": data,
            })))
        }
    }
}

pub(crate) fn map_model_error(error: peri_model::ModelError) -> AgentError {
    if error.is_cancelled() {
        AgentError::Interrupted
    } else {
        AgentError::ModelError(error)
    }
}

/// legacy v1 wire format 的 stop_reason 字符串（JSON 字段值，非 Debug 形式）。
///
/// LlmCallEnd output JSON 的 `stop_reason` 字段依赖它，不能退化为 `{:?}` 的变体名。
pub(crate) fn stop_reason_display(reason: &peri_model::StopReason) -> String {
    match reason {
        peri_model::StopReason::EndTurn => "end_turn".into(),
        peri_model::StopReason::ToolUse => "tool_use".into(),
        peri_model::StopReason::MaxTokens => "max_tokens".into(),
        peri_model::StopReason::Continue => "continue".into(),
        peri_model::StopReason::Other { value } => value.clone(),
    }
}
