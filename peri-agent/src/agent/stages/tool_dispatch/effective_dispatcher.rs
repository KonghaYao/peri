//! Effective tool dispatch over the same pinned catalog and invocation pipeline as Act.
//! Nested calls project effective target identity but do not commit transcript
//! messages or settle the outer batch's hooks/failure counter.
//!
//! 本入口只持有 [`InvocationContext`]（会话事实 + 端口）与 pinned catalog：App/Host
//! 租约会长期持有它，不得反向把 session 挂上来。

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::execution::{collect_tool_results, effective_tool_error, SettledInvocation};
use super::InvocationContext;
use crate::agent::react::ToolCall;
use crate::messages::{BaseMessage, ToolCallRequest};
use crate::session::tool_catalog::SessionToolCatalogSnapshot;
use crate::tools::{
    EffectiveToolCall, EffectiveToolDefinition, EffectiveToolDispatcher, EffectiveToolError,
    EffectiveToolErrorCode, ToolCallId, ToolOutcome, ToolOutput,
};
use peri_acp_types::error::ToolRejectionCause;

#[derive(Clone)]
pub(super) struct StageEffectiveToolDispatcher {
    context: InvocationContext,
    catalog: Arc<SessionToolCatalogSnapshot>,
    model_tool_call_id: Option<ToolCallId>,
}

impl StageEffectiveToolDispatcher {
    pub(super) fn new(
        context: InvocationContext,
        catalog: Arc<SessionToolCatalogSnapshot>,
    ) -> Self {
        Self {
            context,
            catalog,
            model_tool_call_id: None,
        }
    }

    pub(super) fn with_tool_call_id(mut self, tool_call_id: ToolCallId) -> Self {
        self.model_tool_call_id = Some(tool_call_id);
        self
    }

    // `EffectiveToolError` 携带远端 raw 错误载荷（契约层定义），按值返回即超过 clippy
    // 的大错误阈值；装箱属于契约层的决定，这里不重复包装。
    #[allow(clippy::result_large_err)]
    async fn dispatch_result(
        &self,
        call: EffectiveToolCall,
        cancel: CancellationToken,
    ) -> Result<SettledInvocation, EffectiveToolError> {
        // 事件 id 仍表达嵌套链：嵌套执行投影 `outer/inner`，非嵌套使用本次执行身份。
        let event_invocation_id = match call.origin.parent_invocation_id() {
            Some(parent) => format!("{parent}/{}", call.invocation_id),
            None => call.invocation_id.to_string(),
        };
        let raw_call = ToolCall::new(event_invocation_id, call.tool_name.clone(), call.input);
        let all_tools = self.catalog.tool_map();
        let invocation = self
            .context
            .resolver()
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
        collect_tool_results(
            &self
                .context
                .with_single_origin(call.origin)
                .with_cancellation(cancel.clone()),
            vec![invocation.policy_call],
            &event_calls,
            &target_tools,
            &self.catalog,
            &cancel,
            ai_message.id(),
            &ai_message,
            self.model_tool_call_id.as_ref().map(ToolCallId::as_str),
        )
        .await
        .map_err(effective_tool_error)?
        .results
        .into_iter()
        .next()
        .ok_or_else(|| {
            EffectiveToolError::new(
                EffectiveToolErrorCode::ToolFailed,
                "tool call produced no result",
            )
        })
    }
}

#[async_trait::async_trait]
impl EffectiveToolDispatcher for StageEffectiveToolDispatcher {
    async fn dispatch(
        &self,
        call: EffectiveToolCall,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, EffectiveToolError> {
        let SettledInvocation {
            result, outcome, ..
        } = self.dispatch_result(call, cancel.clone()).await?;
        if result.is_error {
            // 已报告结果事实的执行不按协议层失败处理：raw（含远端 `isError`）随
            // outcome 保真返回，由调用方（App relay / 模型面投影）各自解读。
            // 未报告事实的失败（本地工具错误、取消、超时）仍以
            // [`EffectiveToolError`] 返回。
            if let ToolOutcome::Reported { .. } = outcome {
                return Ok(ToolOutput {
                    text: result.output,
                    execution: result.execution,
                    outcome,
                });
            }
            let code = result.effective_error_code.unwrap_or_else(|| {
                if cancel.is_cancelled() {
                    EffectiveToolErrorCode::Cancelled
                } else {
                    EffectiveToolErrorCode::ToolFailed
                }
            });
            let mut error = EffectiveToolError::new(code, result.output);
            // 「无人可答」的 typed 原因随结果事实过边界（裁决 R9）：这一条路径不经过
            // `effective_tool_error`（错误已在审批阶段结算成 `ToolResult`），因此必须
            // 显式还原，否则 App / Host 面会退化成「用户拒绝」。
            if let Some(ToolRejectionCause::ApprovalUnavailable(cause)) = result.rejection_cause {
                error = error.with_unanswered_cause(cause);
            }
            if let Some(failure) = result.subagent_failure {
                error = error.with_subagent_failure(failure);
            }
            return Err(error);
        }
        Ok(ToolOutput {
            text: result.output,
            execution: result.execution,
            outcome,
        })
    }

    fn admitted_mcp_tool_name(&self, server: &str, wire_name: &str) -> Option<String> {
        self.catalog.tools.iter().find_map(|(name, entry)| {
            (entry.tool.mcp_server_name() == Some(server)
                && entry.tool.mcp_tool_name() == Some(wire_name))
            .then(|| name.clone())
        })
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition> {
        self.catalog
            .tools
            .values()
            .map(|entry| &entry.tool)
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
