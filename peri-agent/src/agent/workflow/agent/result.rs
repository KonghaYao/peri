//! 循环/转发终态到 workflow 结果与遥测的投影；不管理任务或事件通道。

use std::{sync::Arc, time::Instant};

use peri_acp_types::{
    command::PromptStopReason,
    session::{ExecutionFailure, TurnTelemetryOutcome},
    workflow::{AgentRunParams, AgentRunResult, Usage},
};
use tracing::{debug, warn};

use super::observation::{RunStats, WorkflowObservation};
use crate::{agent::stages::LoopResult, session::Session};

pub(super) struct ProjectedResult {
    pub result: AgentRunResult,
    failure: Option<ExecutionFailure>,
}

impl ProjectedResult {
    pub fn telemetry_outcome(&self) -> TurnTelemetryOutcome {
        match &self.result {
            AgentRunResult::Dead { reason, .. } if reason.as_deref() == Some("interrupted") => {
                TurnTelemetryOutcome::Stopped {
                    reason: PromptStopReason::Cancelled,
                }
            }
            AgentRunResult::Dead { .. } => TurnTelemetryOutcome::Failed {
                failure: self.failure.clone().unwrap_or_else(|| {
                    ExecutionFailure::internal("Workflow agent execution failed")
                }),
            },
            _ => TurnTelemetryOutcome::Completed,
        }
    }
}

/// 调用方必须先关闭 EventBus 并 await forwarder，再读取最终统计与输出。
pub(super) fn project_run_result(
    loop_result: LoopResult,
    forwarder_result: Result<(), ExecutionFailure>,
    session: &Arc<Session>,
    observation: &WorkflowObservation,
    params: &AgentRunParams,
    effective_model: &str,
    started_at: Instant,
) -> ProjectedResult {
    if let Err(failure) = forwarder_result {
        warn!(message = %failure.public_message, "Workflow agent: event forwarder failed");
        return ProjectedResult {
            result: workflow_forwarder_dead_result(),
            failure: Some(failure),
        };
    }
    match loop_result {
        LoopResult::Completed => {
            let output = crate::session::subagent::extract_last_ai_text(session);
            ProjectedResult {
                result: completed_result(
                    output,
                    observation.snapshot(),
                    params,
                    effective_model,
                    started_at,
                ),
                failure: None,
            }
        }
        LoopResult::Interrupted => {
            debug!("Workflow agent: execution interrupted");
            ProjectedResult {
                result: AgentRunResult::Dead {
                    reason: Some("interrupted".into()),
                    detail: Some("Workflow agent execution was interrupted".into()),
                },
                failure: None,
            }
        }
        LoopResult::Error(error) => {
            debug!(error = %error, "Workflow agent: execution failed");
            ProjectedResult {
                failure: Some(ExecutionFailure::from_agent_error(&error)),
                result: AgentRunResult::Dead {
                    reason: Some("runagent-threw".into()),
                    detail: Some(error.to_string()),
                },
            }
        }
    }
}

fn completed_result(
    output: String,
    stats: RunStats,
    params: &AgentRunParams,
    effective_model: &str,
    started_at: Instant,
) -> AgentRunResult {
    let mut tokens = stats.output_tokens;
    // 保持既有字节长度估算；未收到实际 usage 的非空输出至少记 1 token。
    if tokens == 0 && !output.is_empty() {
        tokens = (output.len() as u64 / 4).max(1);
    }
    let model = reported_model(stats.last_model, effective_model);
    if let Some(schema) = &params.schema {
        if let Err(error) = validate_json_schema(&output, schema) {
            debug!(error = %error, "Workflow agent: schema validation failed");
            return AgentRunResult::Dead {
                reason: Some("no-structured-output".into()),
                detail: Some(error),
            };
        }
    }
    AgentRunResult::Ok {
        output: serde_json::Value::String(output),
        usage: Usage {
            output_tokens: tokens,
        },
        model,
        tool_count: Some(stats.tool_count),
        token_count: Some(tokens),
        phase: params.phase.clone(),
        duration_ms: Some(started_at.elapsed().as_millis() as u64),
    }
}

pub(super) fn workflow_forwarder_dead_result() -> AgentRunResult {
    AgentRunResult::Dead {
        reason: Some("event-forwarder-failed".into()),
        detail: Some("Workflow agent event forwarding failed".into()),
    }
}

pub(super) fn reported_model(last_model: Option<String>, effective_model: &str) -> Option<String> {
    last_model.or_else(|| Some(effective_model.to_string()))
}

/// JSON Schema 校验——基础类型 + required 字段检查。
///
/// 调用时始终验证合法 JSON；空 {} 或非 object schema 不增加字段约束。
/// 未提供 schema 时，调用方跳过本校验。
/// 否则检查：
/// 1. 顶层 type 匹配（object/array/string/number/boolean/null）
/// 2. 若 type 为 object，检查 required 字段存在
/// 3. 若 type 为 object 且有 properties，检查各属性 type 匹配
fn validate_json_schema(text: &str, schema: &serde_json::Value) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("output is not valid JSON: {e}"))?;

    // 如果 schema 为空或不是 object，仅验证 JSON 格式
    let schema_obj = match schema.as_object() {
        Some(obj) if obj.is_empty() => return Ok(()),
        Some(_) => schema,
        _ => return Ok(()),
    };

    // 检查顶层 type
    if let Some(expected_type) = schema_obj.get("type").and_then(|v| v.as_str()) {
        let actual_type = json_type_name(&value);
        if actual_type != expected_type {
            return Err(format!(
                "expected top-level type '{expected_type}', got '{actual_type}'"
            ));
        }
    }

    // 对 object 类型检查 required + properties
    if let Some(obj) = value.as_object() {
        if let Some(required) = schema_obj.get("required").and_then(|v| v.as_array()) {
            for field in required {
                let field_name = field
                    .as_str()
                    .ok_or_else(|| format!("required 数组元素不是字符串: {field}"))?;
                if !obj.contains_key(field_name) {
                    return Err(format!("missing required field: {field_name}"));
                }
            }
        }

        if let Some(properties) = schema_obj.get("properties").and_then(|v| v.as_object()) {
            for (prop_name, prop_schema) in properties {
                if let Some(prop_value) = obj.get(prop_name) {
                    if let Some(expected_type) = prop_schema.get("type").and_then(|v| v.as_str()) {
                        let actual_type = json_type_name(prop_value);
                        if actual_type != expected_type {
                            return Err(format!(
                                "field '{prop_name}': expected type '{expected_type}', got '{actual_type}'"
                            ));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// 返回 JSON value 的类型名称（用于错误消息）。
fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
#[path = "result_test.rs"]
mod tests;
