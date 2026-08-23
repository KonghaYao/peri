use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    IncomingMessage, JsExecutionFailure, JsExecutionHost, JsProcessSpec, JsRuntimeError,
    ResourceKind, Result,
};

const NODE_ADAPTER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../npm-packages/@peri-ptc/src/adapter.js"
));
const NODE_BOOTSTRAP: &str = "startPtcAdapter();\n";
const CLEANUP_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsExecutionRequest {
    pub source: String,
    pub input: Value,
}

impl fmt::Debug for JsExecutionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsExecutionRequest")
            .field("source", &"[REDACTED]")
            .field("input", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsExecutionResult {
    pub value: Value,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct JsExecutionLimits {
    pub wall_timeout: Duration,
    pub max_source_bytes: usize,
    pub max_input_bytes: usize,
    pub max_frame_bytes: usize,
    pub max_logs_bytes: usize,
    pub max_result_bytes: usize,
    pub max_internal_calls: usize,
    pub max_concurrent_executions: usize,
}

impl Default for JsExecutionLimits {
    fn default() -> Self {
        Self {
            wall_timeout: Duration::from_secs(60),
            max_source_bytes: 256 * 1024,
            max_input_bytes: 1024 * 1024,
            max_frame_bytes: 4 * 1024 * 1024,
            max_logs_bytes: 1024 * 1024,
            max_result_bytes: 4 * 1024 * 1024,
            max_internal_calls: 16,
            max_concurrent_executions: 4,
        }
    }
}

impl JsExecutionLimits {
    fn validate(&self) -> Result<()> {
        let values = [
            self.wall_timeout.as_nanos() as usize,
            self.max_source_bytes,
            self.max_input_bytes,
            self.max_frame_bytes,
            self.max_logs_bytes,
            self.max_result_bytes,
            self.max_internal_calls,
            self.max_concurrent_executions,
        ];
        if values.contains(&0) {
            return Err(JsRuntimeError::Rpc(
                "JavaScript execution limits must be non-zero".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
pub trait JsRpcRouter: Send + Sync {
    async fn route(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: CancellationToken,
    ) -> Result<Value>;
}

pub struct JsExecutor {
    program: String,
    limits: JsExecutionLimits,
    execution_slots: Arc<Semaphore>,
}

impl JsExecutor {
    pub fn new(program: impl Into<String>) -> Self {
        Self::with_limits(program, JsExecutionLimits::default()).expect("default limits are valid")
    }

    pub fn with_limits(program: impl Into<String>, limits: JsExecutionLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            program: program.into(),
            execution_slots: Arc::new(Semaphore::new(limits.max_concurrent_executions)),
            limits,
        })
    }

    pub async fn execute(
        &self,
        request: JsExecutionRequest,
        router: Arc<dyn JsRpcRouter>,
        cancel: CancellationToken,
    ) -> Result<JsExecutionResult> {
        self.check_request(&request)?;
        let deadline = tokio::time::Instant::now() + self.limits.wall_timeout;
        let _permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(JsRuntimeError::Cancelled),
            _ = tokio::time::sleep_until(deadline) => return Err(JsRuntimeError::Timeout { limit: self.limits.wall_timeout }),
            permit = self.execution_slots.clone().acquire_owned() => permit.map_err(|_| JsRuntimeError::Rpc("execution semaphore closed".into()))?,
        };

        let spec = JsProcessSpec::new(
            &self.program,
            vec![
                "--input-type=module".into(),
                "--eval".into(),
                format!("{NODE_ADAPTER}\n{NODE_BOOTSTRAP}"),
            ],
        )
        .without_inherited_environment();
        let host = Arc::new(JsExecutionHost::spawn_with_frame_limit(
            spec,
            self.limits.max_frame_bytes,
        )?);
        let outcome = self
            .run(host.clone(), request, router, cancel, deadline)
            .await;
        let cleanup = tokio::time::timeout(
            CLEANUP_GRACE,
            host.terminate_and_wait("JavaScript execution finished", Duration::from_millis(100)),
        )
        .await;
        if outcome.is_ok() {
            match cleanup {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(JsRuntimeError::CleanupFailed("cleanup timed out".into())),
            }
        }
        outcome
    }

    fn check_request(&self, request: &JsExecutionRequest) -> Result<()> {
        check_limit(
            ResourceKind::SourceBytes,
            request.source.len(),
            self.limits.max_source_bytes,
        )?;
        check_limit(
            ResourceKind::InputBytes,
            serde_json::to_vec(&request.input)?.len(),
            self.limits.max_input_bytes,
        )
    }

    async fn run(
        &self,
        host: Arc<JsExecutionHost>,
        request: JsExecutionRequest,
        router: Arc<dyn JsRpcRouter>,
        cancel: CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<JsExecutionResult> {
        let channel = host.channel();
        let mut incoming = host
            .take_incoming()
            .await
            .ok_or_else(|| JsRuntimeError::Rpc("incoming receiver unavailable".into()))?;
        let wire = json!({
            "source": request.source,
            "input": request.input,
            "limits": {
                "maxFrameBytes": self.limits.max_frame_bytes,
                "maxLogsBytes": self.limits.max_logs_bytes,
                "maxResultBytes": self.limits.max_result_bytes,
            }
        });
        let mut request_task = tokio::spawn({
            let channel = channel.clone();
            async move { channel.send_request("execute", wire).await }
        });
        let invocation_cancel = cancel.child_token();
        let internal_slots = Arc::new(Semaphore::new(self.limits.max_internal_calls));
        let mut router_tasks = tokio::task::JoinSet::new();
        let mut request_finished = false;

        let outcome = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break Err(JsRuntimeError::Cancelled),
                _ = tokio::time::sleep_until(deadline) => break Err(JsRuntimeError::Timeout { limit: self.limits.wall_timeout }),
                result = &mut request_task, if !request_finished => {
                    request_finished = true;
                    let value = result
                        .map_err(|_| JsRuntimeError::Rpc("execution request task failed".into()))?
                        .map_err(normalize_execute_response_error)?;
                    let parsed: JsExecutionResult = serde_json::from_value(value)?;
                    check_result(&parsed, &self.limits)?;
                    break Ok(parsed);
                }
                message = incoming.recv() => match message {
                    Some(IncomingMessage::Request { id, method, params }) => {
                        let permit = if method == "tool/call" {
                            match internal_slots.clone().try_acquire_owned() {
                                Ok(permit) => Some(permit),
                                Err(_) => {
                                    if let Some(id) = id {
                                        let _ = channel.send_error(id, -32003, "JavaScript resource limit exceeded", Some(json!({"code": "RESOURCE_LIMIT"}))).await;
                                    }
                                    continue;
                                }
                            }
                        } else { None };
                        let channel = channel.clone();
                        let router = router.clone();
                        let child_cancel = invocation_cancel.child_token();
                        router_tasks.spawn(async move {
                            let _permit = permit;
                            if let Some(id) = id {
                                match router.route(&method, params, child_cancel).await {
                                    Ok(value) => { let _ = channel.send_response(id, value).await; }
                                    Err(error) => {
                                        let code = stable_wire_error_code(&error);
                                        let message = safe_error_message(code);
                                        let _ = channel.send_error(id, -32002, message, Some(json!({"code": code}))).await;
                                    }
                                }
                            }
                        });
                    }
                    Some(IncomingMessage::ResourceLimit { resource, limit, observed }) => break Err(JsRuntimeError::ResourceLimit { resource, limit, observed }),
                    Some(IncomingMessage::ProtocolError(_)) => break Err(JsRuntimeError::Rpc("JavaScript RPC protocol error".into())),
                    Some(_) => {}
                    None => break Err(JsRuntimeError::Rpc("JavaScript process closed stdout".into())),
                }
            }
        };
        invocation_cancel.cancel();
        if !request_finished {
            request_task.abort();
            let _ = request_task.await;
        }
        tokio::task::yield_now().await;
        router_tasks.abort_all();
        while router_tasks.join_next().await.is_some() {}
        outcome
    }
}

fn check_result(result: &JsExecutionResult, limits: &JsExecutionLimits) -> Result<()> {
    check_limit(
        ResourceKind::ResultBytes,
        serde_json::to_vec(&result.value)?.len(),
        limits.max_result_bytes,
    )?;
    let logs = result.logs.iter().map(|log| log.len()).sum();
    check_limit(ResourceKind::LogBytes, logs, limits.max_logs_bytes)
}

fn normalize_execute_response_error(error: JsRuntimeError) -> JsRuntimeError {
    let JsRuntimeError::RpcResponse(remote) = error else {
        return error;
    };
    if remote.code != -32001 {
        return JsRuntimeError::Rpc("untrusted execute error response".into());
    }
    let code = remote
        .data
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|data| data.get("code"))
        .and_then(Value::as_str);
    let failure = match (code, remote.message.as_str()) {
        (Some("TOOL_FAILED"), "JavaScript execution failed") => JsExecutionFailure::ToolFailed,
        (Some("RESOURCE_LIMIT"), "JavaScript resource limit exceeded") => {
            JsExecutionFailure::ResourceLimit
        }
        (Some("TIMEOUT"), "JavaScript execution timed out") => JsExecutionFailure::Timeout,
        (Some("CANCELLED"), "JavaScript execution cancelled") => JsExecutionFailure::Cancelled,
        _ => return JsRuntimeError::Rpc("untrusted execute error response".into()),
    };
    JsRuntimeError::ExecutionFailed(failure)
}

fn check_limit(resource: ResourceKind, observed: usize, limit: usize) -> Result<()> {
    if observed > limit {
        Err(JsRuntimeError::ResourceLimit {
            resource,
            limit,
            observed,
        })
    } else {
        Ok(())
    }
}

fn stable_wire_error_code(error: &JsRuntimeError) -> &str {
    if let JsRuntimeError::RpcResponse(response) = error {
        if let Some(code) = response
            .data
            .as_ref()
            .and_then(|data| data.get("code"))
            .and_then(Value::as_str)
        {
            return match code {
                "UNKNOWN_TOOL" | "INVALID_INPUT" | "PERMISSION_DENIED" | "USER_REJECTED"
                | "CANCELLED" | "TIMEOUT" | "TOOL_FAILED" | "RESOURCE_LIMIT" => code,
                _ => "TOOL_FAILED",
            };
        }
    }
    error.code()
}

fn safe_error_message(code: &str) -> &'static str {
    match code {
        "CANCELLED" => "JavaScript tool call cancelled",
        "TIMEOUT" => "JavaScript tool call timed out",
        "RESOURCE_LIMIT" => "JavaScript resource limit exceeded",
        _ => "JavaScript tool call failed",
    }
}

#[cfg(test)]
#[path = "executor_test.rs"]
mod tests;
