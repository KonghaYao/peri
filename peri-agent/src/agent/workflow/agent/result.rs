//! 循环/转发终态到 workflow 结果与遥测的投影；不管理任务或事件通道。

use std::sync::Arc;
use std::time::Instant;

use peri_acp_types::command::PromptStopReason;
use peri_acp_types::session::ExecutionFailure;
use peri_acp_types::session::TurnTelemetryOutcome;
use peri_acp_types::thread::AgentStatus;
use peri_acp_types::workflow::AgentExecutionLimits;
use peri_acp_types::workflow::AgentRunParams;
use peri_acp_types::workflow::AgentRunResult;
use peri_acp_types::workflow::Usage;
use tracing::{debug, warn};

use crate::agent::stages::LoopResult;
use crate::agent::workflow::agent::observation::{RunStats, WorkflowObservation};
use crate::session::Session;

pub(super) struct ProjectedResult {
    pub result: AgentRunResult,
    /// 本次执行实际解析的上限与用量（结算路径交给宿主写入 attempt）。
    pub execution_limits: AgentExecutionLimits,
    failure: Option<ExecutionFailure>,
}

impl ProjectedResult {
    /// Preserve execution telemetry without losing the independent run-fence signal.
    pub(super) fn after_scope_close(mut self, closed: bool) -> Self {
        self.result = self.result.after_scope_close(closed);
        if !closed && self.failure.is_none() {
            self.failure = Some(ExecutionFailure::internal(
                "Workflow agent scope did not close; outstanding resources are unconfirmed",
            ));
        }
        self
    }

    #[must_use]
    pub fn telemetry_outcome(&self) -> TurnTelemetryOutcome {
        match &self.result {
            // scope 未结清时终态已按失败收口，中断不能再投影为取消。
            AgentRunResult::Dead {
                reason,
                scope_close_incomplete,
                ..
            } if !*scope_close_incomplete && reason.as_deref() == Some("interrupted") => {
                TurnTelemetryOutcome::Stopped {
                    reason: PromptStopReason::Cancelled,
                }
            }
            AgentRunResult::Dead { .. } => TurnTelemetryOutcome::Failed {
                failure: Box::new(self.failure.clone().unwrap_or_else(|| {
                    ExecutionFailure::internal("Workflow agent execution failed")
                })),
            },
            _ => TurnTelemetryOutcome::Completed,
        }
    }
}

/// 本次 run 结果对应的 child 终态。
///
/// 与 sync/background child 的生命周期约定一致（`docs/design/session-durability.md`
/// §5.3）：正常完成写 `Done`，用户取消写 `Cancelled`，其余中断与失败写 `Error`；
/// `work_state` 由 [`peri_acp_types::thread::WorkState::after_agent_terminal`] 派生。
pub(super) fn terminal_status(result: &AgentRunResult) -> AgentStatus {
    match result {
        AgentRunResult::Ok { .. } | AgentRunResult::Skipped => AgentStatus::Done,
        AgentRunResult::Dead { reason, .. } if reason.as_deref() == Some("interrupted") => {
            AgentStatus::Cancelled
        }
        AgentRunResult::Dead { .. } => AgentStatus::Error,
    }
}

/// 作用域未结清时，任何结果都按失败收口为 `Error`。
///
/// sync child 在 close 失败时不写终态；workflow child 必须结算（否则残留
/// `active + working` 会被父会话反复恢复），因此按 `session-durability` §5.3
/// 的 close 失败映射写 `Error + Idle`：保留「资源未完全释放」的事实，同时不再
/// 成为续跑候选。
pub(super) fn terminal_status_after_close(
    result: &AgentRunResult,
    scope_closed: bool,
) -> AgentStatus {
    if scope_closed {
        terminal_status(result)
    } else {
        AgentStatus::Error
    }
}

/// 投影所需的执行上下文（与循环结果无关的本次执行事实）。
pub(super) struct ProjectionInputs<'a> {
    pub session: &'a Arc<Session>,
    pub observation: &'a WorkflowObservation,
    pub params: &'a AgentRunParams,
    pub effective_model: &'a str,
    /// 执行器解析的有效迭代上限与用量；未采集的用量保持 `None`。
    pub execution_limits: AgentExecutionLimits,
    pub started_at: Instant,
}

/// 调用方必须先关闭 EventBus 并 await forwarder，再读取最终统计与输出。
pub(super) fn project_run_result(
    loop_result: LoopResult,
    forwarder_result: Result<(), ExecutionFailure>,
    inputs: ProjectionInputs<'_>,
) -> ProjectedResult {
    let ProjectionInputs {
        session,
        observation,
        params,
        effective_model,
        execution_limits,
        started_at,
    } = inputs;
    if let Err(failure) = forwarder_result {
        warn!(message = %failure.public_message, "Workflow agent: event forwarder failed");
        return ProjectedResult {
            result: dead_result(
                "event-forwarder-failed",
                Some("Workflow agent event forwarding failed".into()),
                started_at,
                Some(observation.snapshot()),
            ),
            failure: Some(failure),
            execution_limits,
        };
    }
    match loop_result {
        LoopResult::Completed | LoopResult::HookStopped { .. } => {
            let output = super::report::extract_output(session);
            ProjectedResult {
                result: completed_result(
                    output,
                    observation.snapshot(),
                    params,
                    effective_model,
                    started_at,
                ),
                failure: None,
                execution_limits,
            }
        }
        LoopResult::Interrupted => {
            debug!("Workflow agent: execution interrupted");
            ProjectedResult {
                result: dead_result(
                    "interrupted",
                    Some("Workflow agent execution was interrupted".into()),
                    started_at,
                    Some(observation.snapshot()),
                ),
                failure: None,
                execution_limits,
            }
        }
        LoopResult::SettlementDue { .. } => {
            debug!("Workflow agent: settlement window expired");
            ProjectedResult {
                result: dead_result(
                    "settlement-due",
                    Some(
                        "Workflow agent stopped admitting new iterations at its settlement deadline"
                            .into(),
                    ),
                    started_at,
                    Some(observation.snapshot()),
                ),
                failure: None,
                execution_limits,
            }
        }
        LoopResult::Error(error) => {
            debug!(error = %error, "Workflow agent: execution failed");
            // 迭代耗尽用稳定 reason 编码上报（上限与用量在 execution_limits 里），
            // 调用方不需要解析错误文案。
            let reason = if matches!(&error, crate::error::AgentError::MaxIterationsExceeded(_)) {
                peri_acp_types::workflow::AGENT_DEAD_REASON_MAX_ITERATIONS
            } else {
                "runagent-threw"
            };
            ProjectedResult {
                failure: Some(ExecutionFailure::from_agent_error(&error)),
                result: dead_result(
                    reason,
                    Some(error.to_string()),
                    started_at,
                    Some(observation.snapshot()),
                ),
                execution_limits,
            }
        }
    }
}

/// Dead attempts must carry measured duration when the host observed execution;
/// `None` stays an explicit "not measured" rather than silently missing metrics.
fn dead_result(
    reason: &str,
    detail: Option<String>,
    started_at: Instant,
    stats: Option<RunStats>,
) -> AgentRunResult {
    AgentRunResult::Dead {
        scope_close_incomplete: false,
        reason: Some(reason.into()),
        detail,
        duration_ms: Some(started_at.elapsed().as_millis() as u64),
        token_count: stats.and_then(|stats| stats.reported_output_tokens()),
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
    if let Some(schema) = &params.schema {
        if let Err(error) = validate_json_schema(&output, schema) {
            debug!(error = %error, "Workflow agent: schema validation failed");
            return dead_result("no-structured-output", Some(error), started_at, Some(stats));
        }
    }
    let model = reported_model(stats.last_model, effective_model);
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

#[cfg(test)]
pub(super) fn test_execution_limits() -> AgentExecutionLimits {
    AgentExecutionLimits {
        iteration_limit: 200,
        iteration_limit_source: peri_acp_types::workflow::IterationLimitSource::Default,
        iterations_used: None,
    }
}

#[cfg(test)]
pub(super) fn workflow_forwarder_dead_result() -> AgentRunResult {
    AgentRunResult::Dead {
        scope_close_incomplete: false,
        reason: Some("event-forwarder-failed".into()),
        detail: Some("Workflow agent event forwarding failed".into()),
        duration_ms: None,
        token_count: None,
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
            let paired_items = items.iter().enumerate();
            for (index, item) in paired_items {
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
