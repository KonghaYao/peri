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

/// JSON Schema 校验——工作流结果实际使用的有限子集。
///
/// 调用时始终验证合法 JSON；空 {} 或非 object schema 不增加字段约束。
/// 未提供 schema 时，调用方跳过本校验。
/// 否则递归检查 type、object 的 required/properties 和 array 的 items。
/// 这不是完整 JSON Schema 实现；其他关键字不参与校验。
fn validate_json_schema(text: &str, schema: &serde_json::Value) -> Result<(), String> {
    let raw_value: Box<serde_json::value::RawValue> =
        serde_json::from_str(text).map_err(|e| format!("output is not valid JSON: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("output is not valid JSON: {e}"))?;

    // 如果 schema 为空或不是 object，仅验证 JSON 格式
    let schema_obj = match schema.as_object() {
        Some(obj) if obj.is_empty() => return Ok(()),
        Some(obj) => obj,
        _ => return Ok(()),
    };

    validate_schema_value(&value, schema_obj, None, Some(raw_value.as_ref()))
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

fn decimal_parts(raw: &str) -> Option<(bool, String, i64)> {
    let (negative, raw) = raw
        .strip_prefix('-')
        .map_or((false, raw), |raw| (true, raw));
    let (mantissa, exponent) = match raw.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (raw, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = String::with_capacity(whole.len() + fraction.len());
    digits.push_str(whole);
    digits.push_str(fraction);
    let first_digit = digits.bytes().position(|digit| digit != b'0');
    let Some(first_digit) = first_digit else {
        return Some((false, "0".into(), 0));
    };
    digits.drain(..first_digit);
    let mut scale = (fraction.len() as i64).checked_sub(exponent)?;
    while digits.ends_with('0') {
        digits.pop();
        scale = scale.checked_sub(1)?;
    }
    Some((negative, digits, scale))
}

fn json_number_is_integer(
    number: &serde_json::Number,
    raw: Option<&serde_json::value::RawValue>,
) -> bool {
    let Some(raw) = raw else {
        return number.as_i64().is_some() || number.as_u64().is_some();
    };
    decimal_parts(raw.get()).is_some_and(|(_, _, scale)| scale <= 0)
}

fn schema_type_matches(
    value: &serde_json::Value,
    expected_type: &str,
    raw: Option<&serde_json::value::RawValue>,
) -> bool {
    match expected_type {
        "number" => value.is_number(),
        "integer" => value
            .as_number()
            .is_some_and(|number| json_number_is_integer(number, raw)),
        _ => json_type_name(value) == expected_type,
    }
}

fn validate_schema_value(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
    field_path: Option<&str>,
    raw: Option<&serde_json::value::RawValue>,
) -> Result<(), String> {
    if let Some(expected_type) = schema.get("type").and_then(|value| value.as_str()) {
        if !schema_type_matches(value, expected_type, raw) {
            let actual_type = json_type_name(value);
            return Err(match field_path {
                Some(field_path) => format!(
                    "field '{field_path}': expected type '{expected_type}', got '{actual_type}'"
                ),
                None => format!("expected top-level type '{expected_type}', got '{actual_type}'"),
            });
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(|value| value.as_array()) {
            for field in required {
                let field_name = field
                    .as_str()
                    .ok_or_else(|| format!("required 数组元素不是字符串: {field}"))?;
                if !object.contains_key(field_name) {
                    return Err(match field_path {
                        Some(parent) => format!("missing required field: {parent}.{field_name}"),
                        None => format!("missing required field: {field_name}"),
                    });
                }
            }
        }

        if let Some(properties) = schema.get("properties").and_then(|value| value.as_object()) {
            let raw_properties = raw.and_then(|raw| {
                serde_json::from_str::<
                    std::collections::HashMap<String, Box<serde_json::value::RawValue>>,
                >(raw.get())
                .ok()
            });
            for (property_name, property_schema) in properties {
                let Some(property_value) = object.get(property_name) else {
                    continue;
                };
                let Some(property_schema) = property_schema.as_object() else {
                    continue;
                };
                let property_path = field_path
                    .map(|parent| format!("{parent}.{property_name}"))
                    .unwrap_or_else(|| property_name.clone());
                let property_raw = raw_properties
                    .as_ref()
                    .and_then(|properties| properties.get(property_name))
                    .map(|raw| raw.as_ref());
                validate_schema_value(
                    property_value,
                    property_schema,
                    Some(&property_path),
                    property_raw,
                )?;
            }
        }
    }

    if let Some(items_schema) = schema.get("items").and_then(|value| value.as_object()) {
        if let Some(items) = value.as_array() {
            let raw_items = raw.and_then(|raw| {
                serde_json::from_str::<Vec<Box<serde_json::value::RawValue>>>(raw.get()).ok()
            });
            for (index, item) in items.iter().enumerate() {
                let item_path = field_path
                    .map(|parent| format!("{parent}[{index}]"))
                    .unwrap_or_else(|| format!("[{index}]"));
                let item_raw = raw_items
                    .as_ref()
                    .and_then(|items| items.get(index))
                    .map(|raw| raw.as_ref());
                validate_schema_value(item, items_schema, Some(&item_path), item_raw)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "result_test.rs"]
mod tests;
