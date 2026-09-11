//! PTC adapter over the same pinned catalog and invocation pipeline as Act.
//! Nested calls project effective target identity but do not commit transcript
//! messages or settle the outer batch's hooks/failure counter.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::execution::{collect_tool_results, effective_tool_error};
use super::StageContext;
use crate::agent::react::ToolCall;
use crate::messages::{BaseMessage, ToolCallRequest};
use crate::session::tool_catalog::SessionToolCatalogSnapshot;
use crate::tools::{
    EffectiveToolCall, EffectiveToolDefinition, EffectiveToolDispatcher, EffectiveToolError,
    EffectiveToolErrorCode, RUN_PTC_CODE_TOOL_NAME,
};

#[derive(Clone)]
pub(super) struct StageEffectiveToolDispatcher {
    context: StageContext,
    catalog: Arc<SessionToolCatalogSnapshot>,
}

impl StageEffectiveToolDispatcher {
    pub(super) fn new(context: StageContext, catalog: Arc<SessionToolCatalogSnapshot>) -> Self {
        Self { context, catalog }
    }
}

#[async_trait::async_trait]
impl EffectiveToolDispatcher for StageEffectiveToolDispatcher {
    async fn dispatch(
        &self,
        call: EffectiveToolCall,
        cancel: CancellationToken,
    ) -> Result<String, EffectiveToolError> {
        if call.tool_name.eq_ignore_ascii_case(RUN_PTC_CODE_TOOL_NAME) {
            return Err(EffectiveToolError::new(
                EffectiveToolErrorCode::ToolFailed,
                format!("{RUN_PTC_CODE_TOOL_NAME} cannot recursively invoke itself"),
            ));
        }
        let event_invocation_id = call
            .parent_invocation_id
            .as_deref()
            .map(|parent| format!("{parent}/{}", call.invocation_id))
            .unwrap_or_else(|| call.invocation_id.clone());
        let raw_call = ToolCall::new(event_invocation_id, call.tool_name.clone(), call.input);
        let all_tools = self.catalog.tool_map();
        let invocation = self
            .context
            .runtime
            .tool_invocation_resolver
            .resolve(&raw_call, &all_tools)
            .map_err(effective_tool_error)?;
        let policy_id = invocation.policy_call.id.clone();
        let event_calls = HashMap::from([(policy_id.clone(), invocation.policy_call.clone())]);
        let target_tools = HashMap::from([(policy_id, invocation.target)]);
        let ai_message = BaseMessage::ai_with_tool_calls(
            String::new(),
            vec![ToolCallRequest::new(
                raw_call.id.clone(),
                raw_call.name.clone(),
                raw_call.input.clone(),
            )],
        );
        let outcome = collect_tool_results(
            &self.context,
            vec![invocation.policy_call],
            &event_calls,
            &target_tools,
            &self.catalog,
            &cancel,
            ai_message.id(),
            &ai_message,
        )
        .await
        .map_err(effective_tool_error)?;
        let result = outcome
            .results
            .into_iter()
            .next()
            .map(|(_, result)| result)
            .ok_or_else(|| {
                EffectiveToolError::new(
                    EffectiveToolErrorCode::ToolFailed,
                    "tool call produced no result",
                )
            })?;
        if result.is_error {
            let code = result.effective_error_code.unwrap_or_else(|| {
                if cancel.is_cancelled() {
                    EffectiveToolErrorCode::Cancelled
                } else {
                    EffectiveToolErrorCode::ToolFailed
                }
            });
            return Err(EffectiveToolError::new(code, result.output));
        }
        Ok(result.output)
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition> {
        self.catalog
            .tools
            .values()
            .map(|entry| &entry.tool)
            .filter(|tool| tool.name() != RUN_PTC_CODE_TOOL_NAME)
            .map(|tool| EffectiveToolDefinition {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: tool.parameters(),
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "effective_dispatcher_test.rs"]
mod tests;
