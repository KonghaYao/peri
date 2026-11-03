use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{ContentBlock, ModelResult, ToolCall};

use super::{request::invalid, response::project_items, ResponsesProfile, PROVIDER_NAME};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplayBinding {
    pub endpoint: String,
    pub model: String,
    pub credential_ref: String,
    pub profile: ResponsesProfile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayState {
    version: u32,
    binding: ReplayBinding,
    items: Vec<Value>,
}

pub(super) fn state_block(binding: &ReplayBinding, items: &[Value]) -> ContentBlock {
    ContentBlock::ProviderState {
        provider: PROVIDER_NAME.to_owned(),
        data: json!({"version": 1, "binding": binding, "items": items}),
    }
}

pub(super) fn replay_items(
    content: &[ContentBlock],
    tool_calls: &[ToolCall],
    binding: &ReplayBinding,
) -> ModelResult<Option<Vec<Value>>> {
    let mut state = None;
    for block in content {
        if let ContentBlock::ProviderState { provider, data } = block {
            if provider != PROVIDER_NAME {
                continue;
            }
            if state.is_some() {
                return Err(invalid(
                    "Assistant contains duplicate Responses replay state",
                ));
            }
            state = Some(
                serde_json::from_value::<ReplayState>(data.clone()).map_err(|error| {
                    invalid("Malformed Responses replay state").with_error(&error)
                })?,
            );
        }
    }
    let Some(state) = state else {
        return Ok(None);
    };
    if state.version != 1 {
        return Err(invalid("Unsupported Responses replay state version"));
    }
    let (projected_content, projected_calls) =
        project_items(&state.items, state.binding.profile, None)?;
    let visible_content: Vec<_> = content
        .iter()
        .filter(|block| {
            !matches!(
                block,
                ContentBlock::ProviderState { .. } | ContentBlock::ToolUse { .. }
            )
        })
        .cloned()
        .collect();
    let embedded_calls: Vec<_> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { tool_call } => Some(tool_call.clone()),
            _ => None,
        })
        .collect();
    let calls = if tool_calls.is_empty() {
        &embedded_calls
    } else {
        tool_calls
    };
    if projected_content != visible_content
        || projected_calls != calls
        || (!embedded_calls.is_empty() && embedded_calls != projected_calls)
    {
        return Err(invalid(
            "Responses replay state no longer matches the assistant text/reasoning/tool projection",
        ));
    }
    // Account, endpoint, model and profile are part of the opaque state domain.
    // Foreign-domain turns retain the ordinary projection, never the payload.
    if state.binding != *binding {
        return Ok(None);
    }
    Ok(Some(state.items))
}
