//! One borrowed index for steps, exchanges and provider protocol closures.
use std::collections::{HashMap, HashSet};

use super::super::projection::ProjectionTarget;
use crate::error::{AgentError, AgentResult};
use crate::messages::{BaseMessage, ContentBlock, MessageContent, MessageId};
use crate::session::transcript::{MessageTranscript, TranscriptEntry};

pub struct Step<'a> {
    pub entry: &'a TranscriptEntry,
    pub exchanges: Vec<usize>,
    pub protocol_dependency: bool,
    pub unknown: bool,
}

pub struct Exchange<'a> {
    pub call_id: &'a str,
    pub tool_name: &'a str,
    pub arguments: &'a serde_json::Value,
    pub step: usize,
    pub result: Option<ResultLocation<'a>>,
}

pub struct ResultLocation<'a> {
    pub entry: &'a TranscriptEntry,
    pub target: ProjectionTarget,
    pub is_error: bool,
    pub content: ResultContent<'a>,
}

pub enum ResultContent<'a> {
    Message(&'a MessageContent),
    Blocks(&'a [ContentBlock]),
    Raw(&'a serde_json::Value),
}

impl ResultContent<'_> {
    pub fn text_chars(&self) -> usize {
        fn blocks_len(blocks: &[ContentBlock]) -> usize {
            blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => text.chars().count(),
                    ContentBlock::ToolResult { content, .. } => blocks_len(content),
                    _ => 0,
                })
                .sum()
        }
        fn raw_len(value: &serde_json::Value) -> usize {
            match value {
                serde_json::Value::String(text) => text.chars().count(),
                serde_json::Value::Array(values) => values.iter().map(raw_len).sum(),
                value if value["type"] == "text" => value["text"]
                    .as_str()
                    .map_or(0, |text| text.chars().count()),
                _ => 0,
            }
        }
        match self {
            Self::Message(MessageContent::Text(text)) => text.chars().count(),
            Self::Message(MessageContent::Blocks(blocks)) => blocks_len(blocks),
            Self::Blocks(blocks) => blocks_len(blocks),
            Self::Message(MessageContent::Raw(values)) => values.iter().map(raw_len).sum(),
            Self::Raw(value) => raw_len(value),
        }
    }

    pub fn preview_text_chars(&self, head: usize, tail: usize) -> usize {
        fn blocks_cost(blocks: &[ContentBlock], head: usize, tail: usize) -> usize {
            blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => {
                        super::planning::preview_chars(text.chars().count(), head, tail)
                    }
                    ContentBlock::ToolResult { content, .. } => blocks_cost(content, head, tail),
                    _ => 0,
                })
                .sum()
        }
        match self {
            Self::Message(MessageContent::Text(text)) => {
                super::planning::preview_chars(text.chars().count(), head, tail)
            }
            Self::Message(MessageContent::Blocks(blocks)) => blocks_cost(blocks, head, tail),
            Self::Blocks(blocks) => blocks_cost(blocks, head, tail),
            // Unknown/raw encodings are never generically truncated by the renderer.
            Self::Message(MessageContent::Raw(_)) | Self::Raw(_) => self.text_chars(),
        }
    }

    pub fn has_unrecoverable_media(&self) -> bool {
        fn block_unsafe(block: &ContentBlock) -> bool {
            match block {
                ContentBlock::Image {
                    source: crate::messages::ImageSource::Url { .. },
                }
                | ContentBlock::Document {
                    source: crate::messages::DocumentSource::Url { .. },
                    ..
                }
                | ContentBlock::Unknown(_)
                | ContentBlock::ProviderState { .. }
                | ContentBlock::Reasoning { .. }
                | ContentBlock::ToolUse { .. } => true,
                ContentBlock::ToolResult { content, .. } => content.iter().any(block_unsafe),
                _ => false,
            }
        }
        fn raw_unsafe(value: &serde_json::Value) -> bool {
            match value {
                serde_json::Value::String(_) => false,
                serde_json::Value::Array(values) => values.iter().any(raw_unsafe),
                value if value["type"] == "text" => false,
                value if value["type"] == "image" || value["type"] == "document" => {
                    value["source"]["type"] != "base64" && value["source"]["type"] != "text"
                }
                _ => true,
            }
        }
        match self {
            Self::Message(MessageContent::Text(_)) => false,
            Self::Message(MessageContent::Blocks(blocks)) => blocks.iter().any(block_unsafe),
            Self::Blocks(blocks) => blocks.iter().any(block_unsafe),
            Self::Message(MessageContent::Raw(values)) => values.iter().any(raw_unsafe),
            Self::Raw(value) => raw_unsafe(value),
        }
    }
}

pub struct HistoryIndex<'a> {
    pub steps: Vec<Step<'a>>,
    pub exchanges: Vec<Exchange<'a>>,
    pub positions: HashMap<MessageId, usize>,
}

fn invalid(detail: impl std::fmt::Display) -> AgentError {
    AgentError::LlmError(format!("Micro protocol index: {detail}"))
}

impl<'a> HistoryIndex<'a> {
    pub fn collect(transcript: &'a MessageTranscript) -> AgentResult<Self> {
        let mut index = Self {
            steps: Vec::new(),
            exchanges: Vec::new(),
            positions: HashMap::new(),
        };
        let mut calls = HashMap::new();
        let mut results = HashMap::new();
        for (position, entry) in transcript.entries().iter().enumerate() {
            if position < transcript.ancestor_len() || transcript.is_excluded(entry.id()) {
                continue;
            }
            index.positions.insert(entry.id(), position);
            let Some(message) = entry.as_message() else {
                continue;
            };
            if let BaseMessage::Ai {
                content,
                tool_calls,
                ..
            } = message
            {
                let step = index.steps.len();
                let mut local = HashMap::new();
                for call in tool_calls {
                    if call.id.is_empty()
                        || local
                            .insert(call.id.as_str(), (call.name.as_str(), &call.arguments))
                            .is_some()
                    {
                        return Err(invalid(format!(
                            "duplicate or missing cached call ID {}",
                            call.id
                        )));
                    }
                }
                let mut block_ids = HashSet::new();
                for (id, name, input) in content.tool_use_blocks() {
                    if id.is_empty() || !block_ids.insert(id) {
                        return Err(invalid(format!("duplicate or missing tool_use ID {id}")));
                    }
                    if let Some((cached_name, cached_input)) = local.get(id) {
                        if *cached_name != name || **cached_input != *input {
                            return Err(invalid(format!(
                                "conflicting representations for call {id}"
                            )));
                        }
                    } else {
                        local.insert(id, (name, input));
                    }
                }
                let (protocol_dependency, unknown) = protocol_features(content)?;
                let mut step_calls = local.into_iter().collect::<Vec<_>>();
                // Stable order independent of HashMap iteration; identity never comes from ordering.
                step_calls.sort_by(|left, right| left.0.cmp(right.0));
                let mut exchanges = Vec::new();
                for (id, (name, arguments)) in step_calls {
                    let ei = index.exchanges.len();
                    if calls.insert(id, ei).is_some() {
                        return Err(invalid(format!(
                            "duplicate call ID in own session scope: {id}"
                        )));
                    }
                    exchanges.push(ei);
                    index.exchanges.push(Exchange {
                        call_id: id,
                        tool_name: name,
                        arguments,
                        step,
                        result: None,
                    });
                }
                index.steps.push(Step {
                    entry,
                    exchanges,
                    protocol_dependency,
                    unknown,
                });
            }
            if let BaseMessage::Tool {
                tool_call_id,
                content,
                is_error,
                execution,
                ..
            } = message
            {
                if results
                    .insert(
                        tool_call_id.as_str(),
                        ResultLocation {
                            entry,
                            target: ProjectionTarget::Message,
                            // Completion proves settlement, not success. ToolOutput may
                            // report typed failure without an is_error outcome flag.
                            is_error: *is_error
                                || execution.as_ref().is_some_and(|evidence| {
                                    matches!(
                                        evidence.status,
                                        crate::tools::ToolExecutionStatus::Failed
                                            | crate::tools::ToolExecutionStatus::Cancelled
                                            | crate::tools::ToolExecutionStatus::TimedOut
                                    )
                                }),
                            content: ResultContent::Message(content),
                        },
                    )
                    .is_some()
                {
                    return Err(invalid(format!("duplicate tool result {tool_call_id}")));
                }
            } else {
                match message.message_content() {
                    MessageContent::Blocks(blocks) => {
                        for (bi, block) in blocks.iter().enumerate() {
                            if let ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                ..
                            } = block
                            {
                                if results
                                    .insert(
                                        tool_use_id,
                                        ResultLocation {
                                            entry,
                                            target: ProjectionTarget::ContentBlock { index: bi },
                                            is_error: *is_error,
                                            content: ResultContent::Blocks(content),
                                        },
                                    )
                                    .is_some()
                                {
                                    return Err(invalid(format!(
                                        "duplicate inline tool result {tool_use_id}"
                                    )));
                                }
                            }
                        }
                    }
                    MessageContent::Raw(values) => {
                        for (bi, value) in values.iter().enumerate() {
                            if value["type"] != "tool_result" {
                                continue;
                            }
                            let id = value["tool_use_id"]
                                .as_str()
                                .filter(|id| !id.is_empty())
                                .ok_or_else(|| invalid("inline result has no call ID"))?;
                            let content = value
                                .get("content")
                                .ok_or_else(|| invalid("inline result has no content"))?;
                            if results
                                .insert(
                                    id,
                                    ResultLocation {
                                        entry,
                                        target: ProjectionTarget::ContentBlock { index: bi },
                                        is_error: value["is_error"].as_bool().unwrap_or(false),
                                        content: ResultContent::Raw(content),
                                    },
                                )
                                .is_some()
                            {
                                return Err(invalid(format!("duplicate raw tool result {id}")));
                            }
                        }
                    }
                    MessageContent::Text(_) => {}
                }
            }
        }
        for (id, result) in results {
            // Ancestor calls are not writable; their own results remain protected.
            let Some(ei) = calls.get(id).copied() else {
                continue;
            };
            let call_entry = index.steps[index.exchanges[ei].step].entry;
            if index.positions[&result.entry.id()] <= index.positions[&call_entry.id()] {
                return Err(invalid(format!("result precedes call {id}")));
            }
            index.exchanges[ei].result = Some(result);
        }
        Ok(index)
    }
}

fn protocol_features(content: &MessageContent) -> AgentResult<(bool, bool)> {
    let mut signed = false;
    let mut unknown = false;
    match content {
        MessageContent::Text(_) => {}
        MessageContent::Blocks(blocks) => {
            for block in blocks {
                match block {
                    ContentBlock::Reasoning { .. } => signed = true,
                    // Opaque replay items cannot be split by generic protocol
                    // retirement. Full compact may replace the whole history.
                    ContentBlock::Unknown(_) | ContentBlock::ProviderState { .. } => unknown = true,
                    _ => {}
                }
            }
        }
        MessageContent::Raw(values) => {
            for value in values {
                match value["type"].as_str() {
                    Some("tool_use") => {
                        if value["id"].as_str().is_none_or(str::is_empty)
                            || value["name"].as_str().is_none()
                            || value.get("input").is_none()
                        {
                            return Err(invalid("raw tool_use has missing identity/input"));
                        }
                    }
                    Some("thinking" | "reasoning" | "redacted_thinking") => signed = true,
                    Some("text" | "tool_result" | "image" | "document") => {}
                    _ => unknown = true,
                }
            }
        }
    }
    Ok((signed, unknown))
}
