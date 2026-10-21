//! One cost scan per current result block. IDs survive earlier sibling omissions.
use super::index::ResultContent;
use crate::agent::token::{estimate_block_chars, estimate_request_tokens};
use crate::messages::{BaseMessage, ContentBlock, MessageContent};
use std::collections::HashMap;

pub(super) fn result_costs(messages: &[BaseMessage]) -> HashMap<String, (usize, u64)> {
    let mut costs = HashMap::new();
    for message in messages {
        if let BaseMessage::Tool {
            tool_call_id,
            content,
            ..
        } = message
        {
            costs.insert(
                tool_call_id.clone(),
                (
                    ResultContent::Message(content).text_chars(),
                    estimate_request_tokens(std::slice::from_ref(message), &[]),
                ),
            );
            continue;
        }
        match message.message_content() {
            MessageContent::Blocks(blocks) => {
                for block in blocks {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = block
                    {
                        costs.insert(
                            tool_use_id.clone(),
                            (
                                ResultContent::Blocks(content).text_chars(),
                                estimate_block_chars(block, &[]).div_ceil(4),
                            ),
                        );
                    }
                }
            }
            MessageContent::Raw(values) => {
                for value in values {
                    if value["type"] != "tool_result" {
                        continue;
                    }
                    let Some(id) = value["tool_use_id"].as_str() else {
                        continue;
                    };
                    let chars = serde_json::from_value::<ContentBlock>(value.clone()).map_or_else(
                        |_| value.to_string().chars().count() as u64,
                        |block| estimate_block_chars(&block, &[]),
                    );
                    costs.insert(
                        id.to_string(),
                        (
                            ResultContent::Raw(&value["content"]).text_chars(),
                            chars.div_ceil(4),
                        ),
                    );
                }
            }
            MessageContent::Text(_) => {}
        }
    }
    costs
}
