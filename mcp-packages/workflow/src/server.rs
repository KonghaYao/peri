//! workflow builtin MCP handler：4 个工具、5 个资源、标准资源订阅。
//!
//! 设计依据：`docs/design/workflow-mcp.md` §4。三层授权边界：
//! 1. **发现过滤**（模型面）由注册表 `BuiltinMcpTool.model_visible` 在宿主桥接点
//!    完成（`WorkflowAgentResult` 是 host-only）；本 handler 与 host 调用面无关。
//! 2. **调用授权**（服务端）：`WorkflowAgentResult` 校验调用请求 `_meta` 中的消费者
//!    令牌（[`WORKFLOW_CONSUMER_META_KEY`]），并要求目标任务已被 claim
//!    （`SettleAdmission::ExecutingOnly`）——不信任调用方参数声明的身份。
//! 3. **执行授权**：任务参数只能收缩消费者权限，由消费者侧过滤链承担（P6）。
//!
//! 工具面错误分两层（见 [`DispatchOutcome`]）：参数结构 / 未知工具 / 调用授权失败
//! 是**协议级**错误（JSON-RPC error，客户端可按错误码分类）；业务失败（run / task
//! 不存在、runtime 返回的启动校验失败、关闭与未就绪屏障）保留**工具级**
//! `is_error` 文本。
//!
//! 订阅只发标准 `notifications/resources/updated`（无 vendor notification）；
//! `get_info` 声明 `resources.subscribe` 是 SDK 受理 `subscriptions/listen` 的前提。

use std::sync::Arc;

use peri_workflow::protocol::AgentRunResult;
use peri_workflow::runtime::{
    SettleOutcome, WorkflowRuntime, WorkflowTaskState, WORKFLOW_CONSUMER_META_KEY,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, RequestMetaObject,
    Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig,
    SubscriptionFilter, Tool,
};
use rmcp::service::{RequestContext, RoleServer, SubscriptionContext};
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use serde_json::{json, Map, Value};

use crate::input::WorkflowInstanceInput;
use crate::resources::{self, ResourceReadError};

/// `workflow` 实例的 `ServerConfig` 名字（实例名仍是注册表里的 `"workflow"`）。
const WORKFLOW_SERVER_NAME: &str = "peri-workflow-mcp";

/// `WorkflowStatus` 的 run 索引默认条数。
const DEFAULT_RUN_INDEX_LIMIT: u64 = 20;
/// `WorkflowStatus` 的 run 索引条数上界。
const MAX_RUN_INDEX_LIMIT: u64 = 100;

/// 订阅等待运行时槽位绑定的轮询间隔。
///
/// 槽位（[`WorkflowInstanceInput`]）只暴露「是否已绑定」的读取面、没有绑定通知面，
/// 因此只能轮询；该等待只覆盖「实例已连接、会话尚未就绪」的短窗口。
/// **移除条件**：槽位提供绑定通知（`watch` / `Notify`）后改为事件等待，删除轮询。
const UNBOUND_WAIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

const STATUS_DESCRIPTION: &str = "Report workflow run status. \
    Without 'runId', returns the most recent run index (bounded by 'limit', default 20). \
    With 'runId', returns that run's snapshot; an unknown run is reported as not found.";

const KILL_DESCRIPTION: &str = "Terminate a workflow run or one of its agents. \
    Provide 'runId' to kill the whole run (including queued agent tasks), \
    or 'runId' plus 'agentId' to kill a single agent and its pending task.";

const AGENT_RESULT_DESCRIPTION: &str = "Host-only: settle the result of a published agent task. \
    The caller must hold the session binding that published the task; \
    unknown or already-settled tasks are answered without side effects.";

/// 取消口径：工具执行期间收到 `notifications/cancelled` ⇒ 工具级错误文本。
///
/// 取消来自调用方自身的中断（非参数违约），且 MCP 没有专用取消错误码；
/// 协议级错误只表达「请求本身不合法」，因此保留 `is_error` 文本。
const CANCELLED_TEXT: &str = "Tool execution cancelled.";

/// 工具分派结果：**协议级错误**与**工具级结果**显式分离。
///
/// - [`DispatchOutcome::Protocol`]：调用方协议违规 ⇒ `tools/call` 返回 JSON-RPC
///   error（`invalid_params`：未知工具 / 参数结构错误 / 调用授权失败 / 未 claim 的
///   回填；`internal_error`：内部不变量破坏），客户端可按错误码分类处理。
/// - [`DispatchOutcome::Tool`]：工具级结果；`Err` 是业务失败（run / task 不存在、
///   runtime 返回的启动校验失败、关闭与未就绪屏障），以 `CallToolResult::is_error`
///   文本返回（沿用既有口径，兼容 TUI/宿主投影）。
enum DispatchOutcome {
    Protocol(McpError),
    Tool(Result<String, String>),
}

impl DispatchOutcome {
    fn tool_ok(text: String) -> Self {
        Self::Tool(Ok(text))
    }

    fn tool_err(text: impl Into<String>) -> Self {
        Self::Tool(Err(text.into()))
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self::Protocol(McpError::invalid_params(message.into(), None))
    }
}

/// 取必填字符串参数：字段缺失 / `null` 与类型错误都是协议违约（`invalid_params`）。
fn required_str<'a>(arguments: &'a Value, field: &str) -> Result<&'a str, McpError> {
    match arguments.get(field) {
        Some(Value::String(value)) => Ok(value.as_str()),
        None | Some(Value::Null) => Err(McpError::invalid_params(
            format!("missing '{field}' argument"),
            None,
        )),
        Some(_) => Err(invalid_argument_type(field, "a string")),
    }
}

/// 取可选字符串参数：字段缺失 / `null` = 未提供；类型错误是协议违约。
fn optional_str<'a>(arguments: &'a Value, field: &str) -> Result<Option<&'a str>, McpError> {
    match arguments.get(field) {
        Some(Value::String(value)) => Ok(Some(value.as_str())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(invalid_argument_type(field, "a string")),
    }
}

/// 取可选非负整数参数：字段缺失 / `null` = 未提供；其余类型（含字符串数字）是
/// 协议违约——不得静默降级（审查 M16：`"agentId": "3"` 曾被当作整 run kill）。
fn optional_u64(arguments: &Value, field: &str) -> Result<Option<u64>, McpError> {
    match arguments.get(field) {
        Some(Value::Number(number)) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid_argument_type(field, "a non-negative integer")),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(invalid_argument_type(field, "a non-negative integer")),
    }
}

fn invalid_argument_type(field: &str, expected: &str) -> McpError {
    McpError::invalid_params(
        format!("invalid '{field}' argument: expected {expected}"),
        None,
    )
}

/// `Workflow` 的 resume-only 判定（设计 §6.3）：`resumeFromRunId` 是字符串且未声明
/// `script` / `scriptPath` ⇒ 走内核 `resume`（state/journal 读取与启动都在内核，
/// 调用方不拼装参数）。
///
/// `resumeFromRunId` 与 `script`/`scriptPath` 同时出现仍是 `start`（journal 缓存
/// 重放的既有语义）；`resumeFromRunId` 类型非法时不在此裁决——参数校验统一归
/// `WorkflowRuntime` 单一权威，保持既有工具级口径（不按错误文本猜协议错误码）。
fn resume_only_run_id(arguments: &Value) -> Option<&str> {
    let run_id = arguments["resumeFromRunId"].as_str()?;
    let has_script =
        arguments["script"].as_str().is_some() || arguments["scriptPath"].as_str().is_some();
    (!has_script).then_some(run_id)
}

/// `workflow` 实例的 handler：工具、资源与订阅（状态全部在 `WorkflowRuntime`）。
///
/// 持[`WorkflowInstanceInput`]槽位而非运行时本体：实例在会话环境装配期 spawn，
/// 早于会话旧链构造 `WorkflowRuntime`（设计 §6.2「实例 spawn → 输入注入 → …」），
/// 因此每次调用按槽位解析，未绑定以明确错误收敛。
pub struct WorkflowMcpServer {
    input: WorkflowInstanceInput,
}

impl WorkflowMcpServer {
    pub fn new(input: WorkflowInstanceInput) -> Self {
        Self { input }
    }

    /// 解析槽位中的运行时；未绑定（会话就绪尚未完成）是**装配时序**错误，
    /// 不伪装成实例缺失或空结果。
    fn runtime(&self) -> Result<&Arc<WorkflowRuntime>, String> {
        self.input.runtime().ok_or_else(|| {
            "workflow runtime is not bound: the session has not finished initialization".to_string()
        })
    }

    /// 工具声明（顺序即声明段顺序；`Workflow` 的 schema 来自 runtime 的单一权威）。
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::new(
                "Workflow",
                peri_workflow::runtime::params::START_TOOL_DESCRIPTION,
                schema_of(peri_workflow::runtime::params::start_tool_parameters()),
            ),
            Tool::new(
                "WorkflowStatus",
                STATUS_DESCRIPTION,
                schema_of(json!({
                    "type": "object",
                    "properties": {
                        "runId": {
                            "type": "string",
                            "description": "Workflow run id. Omit to list the run index."
                        },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": MAX_RUN_INDEX_LIMIT,
                            "description": "Maximum number of runs in the index (default 20)."
                        }
                    },
                    "required": []
                })),
            ),
            Tool::new(
                "WorkflowKill",
                KILL_DESCRIPTION,
                schema_of(json!({
                    "type": "object",
                    "properties": {
                        "runId": { "type": "string", "description": "Workflow run id." },
                        "agentId": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Optional agent id; when present only that agent is killed."
                        }
                    },
                    "required": ["runId"]
                })),
            ),
            Tool::new(
                "WorkflowAgentResult",
                AGENT_RESULT_DESCRIPTION,
                schema_of(json!({
                    "type": "object",
                    "properties": {
                        "runId": { "type": "string", "description": "Workflow run id." },
                        "taskId": { "type": "string", "description": "Agent task id published by this run." },
                        "result": {
                            "type": "object",
                            "description": "AgentRunResult tagged by 'kind' (ok | skipped | dead)."
                        }
                    },
                    "required": ["runId", "taskId", "result"]
                })),
            ),
        ]
    }

    /// 工具分派：未知工具与参数结构错误是协议级；业务失败保留工具级文本。
    async fn dispatch_tool(
        &self,
        name: &str,
        arguments: &Value,
        meta: &RequestMetaObject,
    ) -> DispatchOutcome {
        match name {
            "Workflow" => self.call_workflow(arguments).await,
            "WorkflowStatus" => self.call_status(arguments),
            "WorkflowKill" => self.call_kill(arguments).await,
            "WorkflowAgentResult" => self.call_agent_result(arguments, meta),
            other => DispatchOutcome::invalid_params(format!("unknown workflow tool: {other}")),
        }
    }

    /// `Workflow`：就绪屏障 + 关闭位后启动或恢复。
    ///
    /// resume-only（`resumeFromRunId` 且无 `script`/`scriptPath`）走内核 `resume`
    /// （设计 §6.3「resume = 携带 `resumeFromRunId` 调 `Workflow`」，ACP 不拼装
    /// 参数）；其余输入语义不变，仍走 `start`（含 `resumeFromRunId` + script 的
    /// journal 缓存重放语义）。两条路径共用同一组屏障与错误层级。
    ///
    /// 参数校验归 `WorkflowRuntime` 单一权威，其 `Err(String)` 保持工具级
    /// 业务失败（不按错误文本猜协议错误码）。
    async fn call_workflow(&self, arguments: &Value) -> DispatchOutcome {
        let runtime = match self.runtime() {
            Ok(runtime) => runtime,
            Err(error) => return DispatchOutcome::tool_err(error),
        };
        if runtime.is_closed() {
            return DispatchOutcome::tool_err(
                "workflow instance is closed and no longer accepts new runs",
            );
        }
        if !runtime.consumer_ready() {
            return DispatchOutcome::tool_err(
                "workflow task consumer is not ready: the agent task channel is not wired yet, \
                 retry after session setup completes",
            );
        }
        let outcome = match resume_only_run_id(arguments) {
            Some(run_id) => runtime.resume(run_id).await,
            None => runtime.start_run(arguments.clone()).await,
        };
        match outcome {
            Ok(outcome) => DispatchOutcome::tool_ok(outcome.message()),
            Err(error) => DispatchOutcome::tool_err(error),
        }
    }

    fn call_status(&self, arguments: &Value) -> DispatchOutcome {
        // 参数违约（协议级）先于装配时序错误（工具级）裁决：声明了 `limit` 就必须
        // 是合法整数，即使本次查询走 `runId` 分支（N3：类型显式裁决，不静默回落）。
        let run_id = match optional_str(arguments, "runId") {
            Ok(run_id) => run_id,
            Err(error) => return DispatchOutcome::Protocol(error),
        };
        let limit = match optional_u64(arguments, "limit") {
            Ok(limit) => limit
                .unwrap_or(DEFAULT_RUN_INDEX_LIMIT)
                .clamp(1, MAX_RUN_INDEX_LIMIT) as usize,
            Err(error) => return DispatchOutcome::Protocol(error),
        };
        let runtime = match self.runtime() {
            Ok(runtime) => runtime,
            Err(error) => return DispatchOutcome::tool_err(error),
        };
        if let Some(run_id) = run_id {
            let Some(snapshot) = runtime.run_snapshot(run_id) else {
                return DispatchOutcome::tool_err(format!("workflow run '{run_id}' not found"));
            };
            return encode_json(&snapshot, "workflow status");
        }
        // `runs_snapshot` 已按 `started_at_ms` 升序（旧 → 新），取尾部 N 即最近 N 个。
        let mut runs = runtime.runs_snapshot();
        let start = runs.len().saturating_sub(limit);
        let recent = runs.split_off(start);
        encode_json(&recent, "workflow status")
    }

    async fn call_kill(&self, arguments: &Value) -> DispatchOutcome {
        let run_id = match required_str(arguments, "runId") {
            Ok(run_id) => run_id,
            Err(error) => return DispatchOutcome::Protocol(error),
        };
        let agent_id = match arguments.get("agentId") {
            // 字段缺失（或显式 `null`，等价于未提供）⇒ run 级 kill。
            None | Some(Value::Null) => None,
            Some(value) => match value.as_u64() {
                Some(agent_id) => Some(agent_id),
                // 字符串 `"3"` 等 LLM 常见形态：**不得**静默降级为整 run kill
                // （单个 agent 的终止意图不得扩大为 run + 全部排队任务）。
                None => {
                    return DispatchOutcome::invalid_params(
                        "invalid 'agentId' argument: expected an integer agent id; \
                         omit 'agentId' to kill the whole run",
                    )
                }
            },
        };
        let runtime = match self.runtime() {
            Ok(runtime) => runtime,
            Err(error) => return DispatchOutcome::tool_err(error),
        };
        if let Some(agent_id) = agent_id {
            let killed = runtime.kill_agent(run_id, agent_id).await;
            return DispatchOutcome::tool_ok(if killed {
                format!("Agent {agent_id} in workflow run '{run_id}' killed.")
            } else {
                format!("No active agent {agent_id} in workflow run '{run_id}'.")
            });
        }
        let killed = runtime.kill_run(run_id);
        DispatchOutcome::tool_ok(if killed {
            format!("Workflow run '{run_id}' killed.")
        } else {
            format!("Workflow run '{run_id}' not found.")
        })
    }

    /// `WorkflowAgentResult`：调用授权 + 单次终态提交（幂等）。
    ///
    /// 授权口径（design §4.2 第 2 条）：令牌只从调用请求 `_meta` 取得，不信任参数
    /// 声明的身份；消费者未绑定、缺令牌、令牌不匹配一律协议级拒绝。
    ///
    /// 准入口径（design §5.1）：只接受已被唯一 claim（`executing`）的任务
    /// （`SettleAdmission::ExecutingOnly`）；`published` 任务被外部回填、以及回填
    /// 携带非终态 state，都拒绝且**不改变**记录。已终态任务的重复回填仍幂等返回
    /// 既有终态（首次结果为准）。
    fn call_agent_result(&self, arguments: &Value, meta: &RequestMetaObject) -> DispatchOutcome {
        let runtime = match self.authorize_consumer(meta) {
            Ok(runtime) => runtime,
            Err(error) => return DispatchOutcome::Protocol(error),
        };
        let run_id = match required_str(arguments, "runId") {
            Ok(run_id) => run_id,
            Err(error) => return DispatchOutcome::Protocol(error),
        };
        let task_id = match required_str(arguments, "taskId") {
            Ok(task_id) => task_id,
            Err(error) => return DispatchOutcome::Protocol(error),
        };
        let Some(result_value) = arguments.get("result").filter(|value| !value.is_null()) else {
            return DispatchOutcome::invalid_params("missing 'result' argument");
        };
        let result: AgentRunResult = match serde_json::from_value(result_value.clone()) {
            Ok(result) => result,
            Err(error) => {
                return DispatchOutcome::invalid_params(format!(
                    "invalid 'result' argument: {error}"
                ))
            }
        };
        let (state, reason) = task_state_of(&result);
        match runtime.settle_agent_task(run_id, task_id, state, Some(result), reason) {
            SettleOutcome::Settled(record) => DispatchOutcome::tool_ok(format!(
                "Task '{task_id}' settled as '{}'.",
                state_name(record.state)
            )),
            SettleOutcome::AlreadyTerminal(record) => DispatchOutcome::tool_ok(format!(
                "Task '{task_id}' already settled as '{}' (idempotent).",
                state_name(record.state)
            )),
            SettleOutcome::NotFound => DispatchOutcome::tool_err(format!(
                "unknown workflow task '{task_id}' in run '{run_id}'"
            )),
            // 本 handler 的终态映射只产出终态；走到这里说明映射或任务表契约被破坏，
            // 属服务端不变量问题，不伪装成业务失败文本。
            SettleOutcome::InvalidState(record) => {
                tracing::error!(
                    target: "workflow.mcp",
                    run_id,
                    task_id,
                    task_state = state_name(record.state),
                    "WorkflowAgentResult refused a non-terminal settle state"
                );
                DispatchOutcome::Protocol(McpError::internal_error(
                    format!(
                        "refused to settle task '{task_id}' with non-terminal state '{}'",
                        state_name(record.state)
                    ),
                    None,
                ))
            }
            // 活跃态不满足准入（未 claim 的 `published` 任务）：调用方协议违规。
            SettleOutcome::NotAdmitted(record) => DispatchOutcome::invalid_params(format!(
                "task '{task_id}' is '{}', not 'executing': results are only accepted \
                 from the consumer that claimed the task",
                state_name(record.state)
            )),
        }
    }

    /// 消费者调用令牌校验：令牌只来自请求 `_meta`（[`WORKFLOW_CONSUMER_META_KEY`]），
    /// 参数里的任何身份声明都不参与裁决；错误文本不回显令牌值。返回已解析的运行时，
    /// 供调用方复用同一次槽位解析。
    fn authorize_consumer(
        &self,
        meta: &RequestMetaObject,
    ) -> Result<&Arc<WorkflowRuntime>, McpError> {
        let Some(runtime) = self.input.runtime() else {
            return Err(McpError::invalid_params(
                "workflow consumer is not bound: WorkflowAgentResult is only accepted \
                 from the session consumer",
                None,
            ));
        };
        if !runtime.consumer_bound() {
            return Err(McpError::invalid_params(
                "workflow consumer is not bound: WorkflowAgentResult is only accepted \
                 from the session consumer",
                None,
            ));
        }
        let token = meta
            .0
             .0
            .get(WORKFLOW_CONSUMER_META_KEY)
            .and_then(Value::as_str);
        match token {
            Some(token) if runtime.verify_consumer_token(token) => Ok(runtime),
            Some(_) => Err(McpError::invalid_params(
                "invalid workflow consumer token",
                None,
            )),
            None => Err(McpError::invalid_params(
                format!("missing '{WORKFLOW_CONSUMER_META_KEY}' request metadata"),
                None,
            )),
        }
    }

    /// 资源清单：固定 URI（2 个）+ 模板形状（3 个，`{runId}` 需以实际 id 替换）。
    ///
    /// 模板形状同时经 `resources/templates/list` 声明（标准发现面）；`resources/list`
    /// 里的条目让宿主在只枚举 resources 时也能看到完整资源面。
    fn resource_listing() -> Vec<Resource> {
        vec![
            Resource::new(resources::RUNS_URI, "Workflow runs")
                .with_mime_type("application/json")
                .with_description(
                    "Run index snapshot: status, phases, and agent progress per run.",
                ),
            Resource::new("workflow://runs/{runId}", "Workflow run")
                .with_mime_type("application/json")
                .with_description("Single-run snapshot; replace {runId} with a concrete run id."),
            Resource::new("workflow://runs/{runId}/events", "Workflow run events")
                .with_mime_type("application/json")
                .with_description(
                    "Run event log (progress events, terminal summary); supports ?after=N&limit=M.",
                ),
            Resource::new("workflow://runs/{runId}/tasks", "Workflow run tasks")
                .with_mime_type("application/json")
                .with_description("Agent task events for one run; supports ?after=N&limit=M."),
            Resource::new(resources::TASKS_URI, "Workflow tasks")
                .with_mime_type("application/json")
                .with_description(
                    "Session-level agent task event stream (cross-run); supports ?after=N&limit=M.",
                ),
        ]
    }

    /// 等待运行时槽位绑定；`None` = 等待期间订阅被取消。
    ///
    /// 槽位（[`WorkflowInstanceInput`]）只有「当前是否已绑定」的读取面，没有绑定
    /// 通知面，因此以短间隔轮询等待（见 [`UNBOUND_WAIT_INTERVAL`]）；轮询只覆盖
    /// 「实例已连接、会话尚未就绪」的短窗口。绑定后到 receiver 注册之间发布的唤醒
    /// 信号会丢失——通知不承担可靠投递，读取方在订阅确认后立即 `resources/read`
    /// 建立基线（design §4.3）。
    async fn await_runtime(&self, context: &SubscriptionContext) -> Option<Arc<WorkflowRuntime>> {
        if let Some(runtime) = self.input.runtime() {
            return Some(Arc::clone(runtime));
        }
        tracing::info!(
            target: "workflow.mcp",
            "workflow subscription established before the runtime was bound; waiting for session binding"
        );
        loop {
            tokio::select! {
                _ = context.cancelled() => return None,
                _ = tokio::time::sleep(UNBOUND_WAIT_INTERVAL) => {}
            }
            if let Some(runtime) = self.input.runtime() {
                return Some(Arc::clone(runtime));
            }
        }
    }
}

impl ServerHandler for WorkflowMcpServer {
    // 不覆写 `discover`（§10 R2）。

    fn get_info(&self) -> ServerConfig {
        // `resources.subscribe` 是 SDK 受理 `subscriptions/listen` 并按 filter 放行
        // `notifications/resources/updated` 的必要条件（未声明时 filter 收窄为空）。
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_resources_subscribe()
            .build();
        ServerConfig::new(capabilities).with_server_info(Implementation::new(
            WORKFLOW_SERVER_NAME,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let arguments = Value::Object(request.arguments.clone().unwrap_or_default());
        let outcome = tokio::select! {
            biased;
            _ = context.ct.cancelled() => DispatchOutcome::Tool(Err(CANCELLED_TEXT.to_string())),
            outcome = self.dispatch_tool(request.name.as_ref(), &arguments, &context.meta) => outcome,
        };
        match outcome {
            // 协议级错误：JSON-RPC error 响应（客户端按错误码分类），不是工具结果。
            DispatchOutcome::Protocol(error) => Err(error),
            DispatchOutcome::Tool(Ok(text)) => {
                Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text(text),
                ])))
            }
            DispatchOutcome::Tool(Err(text)) => {
                Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                    ContentBlock::text(text),
                ])))
            }
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(Self::resource_listing()))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new("workflow://runs/{runId}", "Workflow run"),
            ResourceTemplate::new("workflow://runs/{runId}/events", "Workflow run events"),
            ResourceTemplate::new("workflow://runs/{runId}/tasks", "Workflow run tasks"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        // 未绑定 = 会话就绪未完成（生产时序：绑定先于会话可见）；资源面不伪造空结果。
        let runtime = self
            .runtime()
            .map_err(|error| McpError::internal_error(error, None))?;
        let payload =
            resources::read_payload(runtime, &request.uri).map_err(|error| match error {
                ResourceReadError::NotSupported(uri) => {
                    McpError::invalid_params(format!("unsupported workflow resource: {uri}"), None)
                }
                ResourceReadError::NotFound(uri) => McpError::resource_not_found(
                    format!("workflow resource not found: {uri}"),
                    None,
                ),
            })?;
        let text = serde_json::to_string_pretty(&payload)
            .map_err(|_| McpError::internal_error("workflow resource encoding failed", None))?;
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![ResourceContents::text(text, request.uri.clone())
                .with_mime_type("application/json")],
        )))
    }

    /// 订阅接受面：接受客户端请求中属于本实例 scheme 的资源 URI。
    ///
    /// 动态 URI（`workflow://runs/{id}*`）无法在服务端预列，因此在 scheme 前缀上
    /// 放行；`SubscriptionSink` 自身按精确 URI 校验每条通知，客户端只会收到它
    /// 显式订阅的 URI。
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let accepted: Vec<String> = requested
            .resource_subscriptions
            .as_ref()?
            .iter()
            .filter(|uri| uri.starts_with(resources::WORKFLOW_URI_PREFIX))
            .cloned()
            .collect();
        if accepted.is_empty() {
            return None;
        }
        Some(
            SubscriptionFilter::builder()
                .resource_subscriptions(accepted)
                .build(),
        )
    }

    /// 订阅生命周期：唤醒信号逐条转发为 `notifications/resources/updated`。
    ///
    /// 通知只是唤醒信号（不承担可靠投递）：广播滞后直接跳过，读取方经
    /// `resources/read` 按游标补读；单条发给未订阅 URI 的尝试被 SDK 拒绝，
    /// 跳过而不终止订阅。
    ///
    /// **订阅先于绑定**（生产时序：`activate()` 之后池初始化即 `subscriptions/listen`，
    /// 而槽位绑定在会话发布前（F2））：此时等待绑定再注册 receiver。SDK 在 handler
    /// 执行前已发出 acknowledge，绑定前放弃注册会让客户端看到「连接正常、通知永远
    /// 不来」的静默失效；等待可取消，连接关闭 / 客户端取消订阅不遗留后台任务。
    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        let Some(runtime) = self.await_runtime(&context).await else {
            // 等待期间订阅被取消：不注册、不伪造唤醒。
            return Ok(());
        };
        let mut updates = runtime.subscribe();
        loop {
            tokio::select! {
                _ = context.cancelled() => break,
                update = updates.recv() => match update {
                    Ok(update) => {
                        let uri = resources::update_uri(&update);
                        match context.sink().notify_resource_updated(uri).await {
                            Ok(()) => {}
                            // 该 URI 未被本订阅接受：跳过，不终止。
                            Err(rmcp::service::SubscriptionSendError::NotificationNotAccepted(_)) => {}
                            // 订阅已关闭 / 连接断开：收口。
                            Err(_) => break,
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
        Ok(())
    }
}

/// 任务终态映射（design §5.3：`cancelled` 以 `Skipped` / `Dead` 形状编码）：
///
/// - `Ok` —— 唯一正常产出形状 ⇒ `completed`；
/// - `Skipped` —— 未执行 / 被跳过，与「取消」同形 ⇒ `cancelled`；
/// - `Dead` —— 执行中断且不可恢复（带原生 reason）⇒ `failed`；显式取消路径走
///   `WorkflowKill`（任务表 CAS 先到者为准），不依赖回填形状区分。
fn task_state_of(result: &AgentRunResult) -> (WorkflowTaskState, Option<String>) {
    match result {
        AgentRunResult::Ok { .. } => (WorkflowTaskState::Completed, None),
        AgentRunResult::Skipped => (WorkflowTaskState::Cancelled, Some("skipped".to_string())),
        AgentRunResult::Dead { reason, .. } => (WorkflowTaskState::Failed, reason.clone()),
    }
}

fn state_name(state: WorkflowTaskState) -> &'static str {
    match state {
        WorkflowTaskState::Published => "published",
        WorkflowTaskState::Executing => "executing",
        WorkflowTaskState::Completed => "completed",
        WorkflowTaskState::Failed => "failed",
        WorkflowTaskState::Cancelled => "cancelled",
    }
}

fn schema_of(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// 序列化工具结果正文：投影类型都是可序列化数据，编码失败属服务端内部错误
/// （不是业务失败，也不是调用方参数问题）。
fn encode_json<T: serde::Serialize>(value: &T, what: &str) -> DispatchOutcome {
    match serde_json::to_string_pretty(value) {
        Ok(text) => DispatchOutcome::tool_ok(text),
        Err(error) => DispatchOutcome::Protocol(McpError::internal_error(
            format!("{what} encoding failed: {error}"),
            None,
        )),
    }
}

#[cfg(test)]
#[path = "server_test_fixture.rs"]
mod test_fixture;

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
