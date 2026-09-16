use peri_acp_types::tasks::TaskManager;
use peri_agent::middleware::capabilities as hook_state;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, LazyLock, Mutex, RwLock},
};

use async_trait::async_trait;
use peri_agent::{
    middleware::r#trait::Middleware,
    tools::{
        BaseTool, EffectiveToolCall, EffectiveToolDefinition, EffectiveToolDispatcher, ToolContext,
        RUN_PTC_CODE_TOOL_NAME,
    },
};
use peri_js_runtime::{
    JsExecutionRequest, JsExecutor, JsRpcRouter, JsRuntimeError, Result as JsResult,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunPtcCodeInput {
    source: String,
    #[serde(default)]
    input: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallParams {
    invocation_id: String,
    tool_name: String,
    input: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCancelParams {
    invocation_id: String,
}

const MAX_PRE_CANCELLED_INVOCATIONS: usize = 256;
static PTC_EXECUTOR: LazyLock<JsExecutor> = LazyLock::new(|| JsExecutor::new("node"));

#[derive(Default)]
struct InvocationState {
    active: HashMap<String, CancellationToken>,
    pre_cancelled: VecDeque<String>,
    completed: VecDeque<String>,
}

impl InvocationState {
    fn cancel(&mut self, invocation_id: String) {
        if let Some(cancel) = self.active.get(&invocation_id) {
            cancel.cancel();
            return;
        }
        if self.completed.contains(&invocation_id) || self.pre_cancelled.contains(&invocation_id) {
            return;
        }
        if self.pre_cancelled.len() == MAX_PRE_CANCELLED_INVOCATIONS {
            self.pre_cancelled.pop_front();
        }
        self.pre_cancelled.push_back(invocation_id);
    }

    fn register(&mut self, invocation_id: &str, cancel: CancellationToken) {
        if let Some(index) = self
            .pre_cancelled
            .iter()
            .position(|cancelled| cancelled == invocation_id)
        {
            self.pre_cancelled.remove(index);
            cancel.cancel();
        }
        self.active.insert(invocation_id.to_string(), cancel);
    }

    fn complete(&mut self, invocation_id: &str) {
        self.active.remove(invocation_id);
        if self.completed.len() == MAX_PRE_CANCELLED_INVOCATIONS {
            self.completed.pop_front();
        }
        self.completed.push_back(invocation_id.to_string());
    }
}

struct PtcRouter {
    dispatcher: Arc<dyn EffectiveToolDispatcher>,
    parent_invocation_id: String,
    invocations: Mutex<InvocationState>,
}

#[async_trait]
impl JsRpcRouter for PtcRouter {
    async fn route(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: CancellationToken,
    ) -> JsResult<Value> {
        match method {
            "tool/cancel" => {
                let params: ToolCancelParams =
                    serde_json::from_value(params.unwrap_or(Value::Null))?;
                self.invocations
                    .lock()
                    .unwrap()
                    .cancel(params.invocation_id);
                Ok(Value::Null)
            }
            "tool/call" => {
                let params: ToolCallParams = serde_json::from_value(params.unwrap_or(Value::Null))?;
                let invocation_id = params.invocation_id.clone();
                self.invocations
                    .lock()
                    .unwrap()
                    .register(&invocation_id, cancel.clone());
                let result = self
                    .dispatcher
                    .dispatch(
                        EffectiveToolCall {
                            invocation_id: params.invocation_id,
                            tool_name: params.tool_name,
                            input: params.input,
                            parent_invocation_id: Some(self.parent_invocation_id.clone()),
                        },
                        cancel,
                    )
                    .await;
                self.invocations.lock().unwrap().complete(&invocation_id);
                result.map(Value::String).map_err(|error| {
                    JsRuntimeError::RpcResponse(peri_js_runtime::JsonRpcError {
                        code: -32002,
                        message: "JavaScript tool call failed".into(),
                        data: Some(json!({ "code": error.code.as_str() })),
                    })
                })
            }
            _ => Err(JsRuntimeError::Rpc(format!(
                "unsupported PTC method: {method}"
            ))),
        }
    }
}

fn format_run_ptc_code_error(error: JsRuntimeError) -> Box<dyn std::error::Error + Send + Sync> {
    format!("{}: {}", error.code(), error.public_message()).into()
}

#[derive(Default)]
pub struct RunPtcCodeTool {
    task_manager: Option<Arc<dyn TaskManager>>,
}

impl RunPtcCodeTool {
    pub fn with_task_manager(mut self, manager: Arc<dyn TaskManager>) -> Self {
        self.task_manager = Some(manager);
        self
    }
}

#[async_trait]
impl BaseTool for RunPtcCodeTool {
    fn name(&self) -> &str {
        RUN_PTC_CODE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Programmatically run code to call tools.<ToolName>(input) from an async ESM function body in a Node.js process（程序化、批量、并发工具调用）. Use JavaScript to batch and concurrently orchestrate multiple tool calls, for example with Promise.all. Top-level await and return are supported; require and static import are unavailable. This tool is permissioned like Bash. tools.* calls use Peri's effective-tool Permission/HITL path; direct Node.js APIs do not and are not sandboxed."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "source": { "type": "string", "description": "An async ESM function body for programmatic, batch, and concurrent tools.<ToolName>(input) calls in a normal Node.js process. Top-level await and return are supported, but require and static import are unavailable. Use Promise.all for concurrent tool calls and `await import('node:...')` for built-ins. Direct Node APIs are not sandboxed. Return a JSON-compatible value. Never read or expose secrets." },
                "input": { "description": "Optional JSON value exposed to the program as input; never include secrets." }
            },
            "required": ["source"]
        })
    }

    fn is_direct(&self) -> bool {
        false
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    async fn invoke(
        &self,
        input: Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let input: RunPtcCodeInput = serde_json::from_value(input)?;
        let dispatcher = ctx.effective_tool_dispatcher.ok_or_else(|| {
            format!("{RUN_PTC_CODE_TOOL_NAME} requires the current Agent effective-tool dispatcher")
        })?;
        let parent_invocation_id = ctx.invocation_id.ok_or_else(|| {
            format!("{RUN_PTC_CODE_TOOL_NAME} requires the current outer tool invocation ID")
        })?;
        let router = Arc::new(PtcRouter {
            dispatcher,
            parent_invocation_id,
            invocations: Mutex::new(InvocationState::default()),
        });
        let cwd = ctx.cwd.to_owned();
        let cancel = ctx.cancellation.child_token();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let manager = self.task_manager.clone();
        let (completed, result) = tokio::sync::oneshot::channel();
        let execution = async move {
            let result = async {
                let mut owner = manager
                    .as_ref()
                    .map(|manager| manager.begin_external_execution())
                    .transpose()
                    .map_err(JsRuntimeError::SpawnFailed)?;
                let shutdown = manager
                    .as_ref()
                    .and_then(|manager| manager.execution_cancel_token());
                let run = PTC_EXECUTOR.execute_in_directory(
                    JsExecutionRequest {
                        source: input.source,
                        input: input.input,
                    },
                    router,
                    cancel.clone(),
                    &cwd,
                );
                tokio::pin!(run);
                let shutdown = async {
                    match shutdown {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                };
                let result = tokio::select! {
                    result = &mut run => result,
                    _ = shutdown => {
                        cancel.cancel();
                        run.await
                    }
                };
                if !matches!(&result, Err(JsRuntimeError::CleanupFailed(_))) {
                    if let Some(owner) = &mut owner {
                        owner.confirm_stopped();
                    }
                }
                result
            }
            .await;
            let _ = completed.send(result);
        };
        if let Some(manager) = &self.task_manager {
            manager.spawn_owned(Box::pin(execution))?;
        } else {
            execution.await;
        }
        let result = result
            .await
            .map_err(|_| "PTC execution ended before cleanup result")?
            .map_err(format_run_ptc_code_error)?;
        Ok(serde_json::to_string(&result)?)
    }
}

pub struct PtcMiddleware {
    cached_contribution: Arc<RwLock<Option<String>>>,
    task_manager: Option<Arc<dyn TaskManager>>,
}

impl PtcMiddleware {
    pub fn new() -> Self {
        Self {
            cached_contribution: Arc::new(RwLock::new(None)),
            task_manager: None,
        }
    }

    pub fn with_task_manager(mut self, manager: Arc<dyn TaskManager>) -> Self {
        self.task_manager = Some(manager);
        self
    }
}

impl Default for PtcMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for PtcMiddleware {
    fn name(&self) -> &str {
        "PtcMiddleware"
    }

    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(RunPtcCodeTool {
            task_manager: self.task_manager.clone(),
        })]
    }

    async fn before_agent(
        &self,
        state: &mut dyn hook_state::BeforeAgentState,
    ) -> peri_agent::error::AgentResult<()> {
        let mut catalog: Vec<EffectiveToolDefinition> = state
            .local_tools()
            .map(|tools| {
                tools
                    .read()
                    .values()
                    .filter(|tool| tool.name() != RUN_PTC_CODE_TOOL_NAME)
                    .map(|tool| EffectiveToolDefinition {
                        name: tool.name().to_string(),
                        description: tool.description().to_string(),
                        parameters: tool.parameters(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        catalog.sort_by(|left, right| left.name.cmp(&right.name));
        let catalog = serde_json::to_string(&catalog).map_err(|error| {
            peri_agent::error::AgentError::MiddlewareError {
                middleware: self.name().to_string(),
                reason: error.to_string(),
            }
        })?;
        *self.cached_contribution.write().unwrap() = Some(format!(
            "{RUN_PTC_CODE_TOOL_NAME} programmatically executes batch and concurrent tools.<ToolName>(input) calls from an async ESM function body in a normal Node.js process and is not a sandbox. Top-level await, return, Promise.all, and dynamic `await import('node:...')` are supported; require and static import are unavailable. tools.* calls use session-local visibility and effective-name Permission/HITL. Direct Node.js filesystem, process, environment, and network APIs do not pass through tools.* Permission/HITL. Never read or expose secrets in source, input, console, return values, exceptions, or tool arguments. RPC-callable tool catalog: {catalog}"
        ));
        Ok(())
    }

    fn prompt_contribution(&self) -> Option<String> {
        self.cached_contribution.read().unwrap().clone()
    }
}

pub fn stable_tool_catalog(
    dispatcher: &dyn EffectiveToolDispatcher,
) -> Vec<EffectiveToolDefinition> {
    let mut tools = dispatcher.tools();
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools
}

#[cfg(test)]
#[path = "ptc_test.rs"]
mod tests;
