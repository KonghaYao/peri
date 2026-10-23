use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::tool_outcome::RawToolResult;
use peri_agent::tools::{BaseTool, ToolExecutionStatus};
use rmcp::model::{ContentBlock, Tool};
use thiserror::Error;

use super::client::output_store::format_output;
use super::client::{McpClientHandle, McpClientPool};

/// MCP 工具调用错误
#[derive(Debug, Error)]
pub enum ToolCallError {
    #[error("MCP 服务器 \"{server}\" 未连接 (状态: {status:?})")]
    NotConnected { server: String, status: String },
    #[error("MCP server \"{server}\" is draining or closed")]
    Unavailable { server: String },
    #[error("MCP 服务器 \"{server}\" 工具 \"{tool}\" 调用失败: {reason}")]
    CallFailed {
        server: String,
        tool: String,
        reason: String,
    },
    #[error("MCP 服务器 \"{server}\" 工具 \"{tool}\" 调用超时 ({timeout_secs}s)")]
    Timeout {
        server: String,
        tool: String,
        timeout_secs: u64,
    },
}

/// 将单个 MCP tool 包装为 BaseTool 实现
///
/// `Clone` 只复制已有的 String/Value/Arc/gate 字段，不建立新连接、不注册新 lease；
/// 供准入路径从已验证快照多次产出 Box，避免重复注册同一工具。
#[derive(Clone)]
pub struct McpToolBridge {
    server_name: String,
    tool_name: String,
    full_name: String,
    description: String,
    input_schema: serde_json::Value,
    model_visible: bool,
    /// 是否直接进入模型 tools 参数；缺省 false（deferred）。
    direct: bool,
    server_generation: u64,
    client: Arc<McpClientHandle>,
    binding_leases: Option<Arc<super::apps::McpAppBindingRegistry>>,
    admission: Option<super::connection_gate::McpCallGate>,
    output_pool: Option<std::sync::Weak<McpClientPool>>,
    output_session_id: Option<String>,
}

/// External MCP requests share a 120-second deadline across send and response.
/// Only the workspace builtin retains its native file/shell deadlines. Identity
/// comes from ConfigSource; other builtins retain the bounded MCP call contract.
/// In particular Bash must finish promotion and return its receipt at the 120s boundary.
pub(crate) const TOOL_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// 从带内执行事实中提取仍在运行的任务 id；工具文本永不参与解析。
fn running_task_id(result: &rmcp::model::CallToolResult) -> Option<String> {
    let value = result
        .meta
        .as_ref()?
        .0
        .get(peri_acp_types::tools::TOOL_EXECUTION_META_KEY)?;
    let evidence =
        serde_json::from_value::<peri_agent::tools::ToolExecutionEvidence>(value.clone()).ok()?;
    matches!(
        evidence.status,
        ToolExecutionStatus::Running | ToolExecutionStatus::RunningAfterTimeout
    )
    .then_some(evidence.task_id)
    .flatten()
}

/// Sanitize name components to match API tool name pattern: ^[a-zA-Z0-9_-]+$
pub(crate) fn sanitize_name_component(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn app_allowed_tools(
    server_name: &str,
    resource_uri: &str,
    tools: &[Tool],
    dispatcher: &dyn peri_acp_types::tools::EffectiveToolDispatcher,
) -> std::collections::HashMap<String, String> {
    tools
        .iter()
        .filter(|tool| {
            super::apps::tool_visibility(tool).app
                && super::apps::tool_resource_uri(tool).as_deref() == Some(resource_uri)
        })
        .filter_map(|tool| {
            let name = tool.name.to_string();
            dispatcher
                .admitted_mcp_tool_name(server_name, &name)
                .map(|effective| (name, effective))
        })
        .collect()
}

pub(crate) fn effective_mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_name_component(server_name),
        sanitize_name_component(tool_name)
    )
}

/// 单个外部 MCP 工具 description 的字节预算（M7）。
///
/// 描述来自外部 server（不受本仓库控制），无预算时一条巨型描述会挤占每次
/// 请求的 tools 参数。超预算按 UTF-8 边界截断，并在**预算内**追加截断标记
/// （标记计入预算，模型看到的是有界文本且知道被截断）。
const MAX_TOOL_DESCRIPTION_BYTES: usize = 8 * 1024;

/// 构造模型可见的工具描述：`[MCP:{server}] {description}`，超预算时截断。
fn bounded_tool_description(server_name: &str, description: Option<&str>) -> String {
    let text = format!("[MCP:{}] {}", server_name, description.unwrap_or(""));
    if text.len() <= MAX_TOOL_DESCRIPTION_BYTES {
        return text;
    }
    let total = text.len();
    let marker = format!("… [description truncated: {total} bytes total]");
    let budget = MAX_TOOL_DESCRIPTION_BYTES.saturating_sub(marker.len());
    let mut truncated = peri_agent::agent::async_tasks::truncate_bytes(&text, budget);
    truncated.push_str(&marker);
    truncated
}

/// builtin 声明表中声明为 host-only（`model_visible: false`）的工具。
///
/// 判定要求来源确为 builtin（`ConfigSource::Builtin` 且实例身份与 server name
/// 一致）：保留名接管在加载期已拒绝，此处仍按来源判定，不依赖配置校验的间接
/// 结论，外部 / 动态 server 因此永不受声明表影响。声明表之外恒 `false`
/// （可见性默认开放，保守默认不改变既有实例行为）。
pub(crate) fn declared_model_hidden(client: &McpClientHandle, tool_name: &str) -> bool {
    matches!(
        &client.source,
        Some(super::config::ConfigSource::Builtin { instance }) if instance == &client.name
    ) && super::builtin::is_declared_model_hidden(&client.name, tool_name)
}

impl McpToolBridge {
    pub fn new(server_name: &str, tool: &Tool, client: Arc<McpClientHandle>) -> Self {
        let tool_name = tool.name.to_string();
        let full_name = effective_mcp_tool_name(server_name, &tool_name);
        let description =
            bounded_tool_description(server_name, tool.description.as_ref().map(|d| d.as_ref()));
        let input_schema = serde_json::to_value(&*tool.input_schema)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        let model_visible =
            super::apps::tool_visibility(tool).model && !declared_model_hidden(&client, &tool_name);
        Self {
            server_name: server_name.to_string(),
            tool_name,
            full_name,
            description,
            input_schema,
            model_visible,
            direct: false,
            server_generation: 0,
            admission: Some(client.call_gate.clone()),
            client,
            binding_leases: None,
            output_pool: None,
            output_session_id: None,
        }
    }

    pub fn with_output_store(
        mut self,
        pool: &Arc<McpClientPool>,
        session_id: Option<&str>,
    ) -> Self {
        self.output_pool = Some(Arc::downgrade(pool));
        self.output_session_id = session_id.map(str::to_string);
        self
    }

    pub fn with_server_generation(mut self, generation: u64) -> Self {
        self.server_generation = generation;
        self
    }

    pub fn with_binding_leases(
        mut self,
        registry: Arc<super::apps::McpAppBindingRegistry>,
    ) -> Self {
        self.binding_leases = Some(registry);
        self
    }

    /// 仅启动清单选中的 system 工具使用原始模型名；wire 身份始终保存在 tool_name。
    pub(crate) fn with_system_direct(mut self) -> Self {
        self.direct = true;
        self.full_name = self.tool_name.clone();
        self
    }

    /// The explicitly selected HTTP Workspace publishes its complete model tool
    /// surface through the live tools/list response.
    pub(crate) fn with_workspace_direct(self) -> Self {
        self.with_system_direct()
    }

    /// MCP 声明的原始工具名（未净化、未加 server 前缀）。
    ///
    /// effective name 的净化不可逆（分隔符与 `__` 都可能出现在分量内），
    /// 需要按原始名匹配时必须经此访问器，不得反拆 `name()`。
    /// server identity 用 [`BaseTool::mcp_server_name`]。
    ///
    /// 调用点归 `system_tools::prepare_system_tools`（同 Wave 落地）。
    pub(crate) fn original_tool_name(&self) -> &str {
        &self.tool_name
    }

    /// 本 bridge 绑定的连接句柄（session-owned 入口的提交复核用它绑定 peer）。
    ///
    /// 只暴露句柄引用：wire 调用仍必须经 `invoke_output`（超时 / 输出预算 /
    /// 任务提升等语义在 bridge 内，不得绕过）。
    pub(crate) fn client_handle(&self) -> &Arc<McpClientHandle> {
        &self.client
    }
}

#[async_trait]
impl BaseTool for McpToolBridge {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    fn mcp_server_name(&self) -> Option<&str> {
        Some(&self.server_name)
    }

    fn mcp_tool_name(&self) -> Option<&str> {
        Some(&self.tool_name)
    }

    fn builtin_mcp_instance(&self) -> Option<&str> {
        match &self.client.source {
            Some(super::config::ConfigSource::Builtin { instance }) => Some(instance),
            _ => None,
        }
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    fn visible_to_model(&self) -> bool {
        self.model_visible
    }

    fn is_direct(&self) -> bool {
        self.direct
    }

    /// 文本投影落点：只返回模型/UI 投影文本。MCP 的 canonical 落点是
    /// [`BaseTool::invoke_output`]，本方法在其上丢弃 raw 与 `isError` 事实。
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.invoke_output(input, ctx).await?.text)
    }

    /// MCP adapter 的 canonical 落点：**先保存 outcome，再投影**。
    ///
    /// 远端 `tools/call` 结果以 [`RawToolResult`] 保真随 `outcome` 返回（`content`
    /// 含多模态块、`structuredContent`、`_meta`、`isError`），调用方（App relay /
    /// ACP / 模型面）各自从返回值解读，不得经旁路 registry 捞取。
    ///
    /// `isError: true` 是工具报告的**结果事实**而不是调用失败：它作为
    /// [`ToolOutcome::Reported`] 的正常返回值离开本函数。只有 transport/协议层失败
    /// 才返回 `Err`。
    async fn invoke_output(
        &self,
        input: serde_json::Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<peri_agent::tools::ToolOutput, Box<dyn std::error::Error + Send + Sync>> {
        let _permit = match &self.admission {
            Some(gate) => Some(gate.try_acquire().map_err(|_| {
                Box::new(ToolCallError::Unavailable {
                    server: self.server_name.clone(),
                }) as Box<dyn std::error::Error + Send + Sync>
            })?),
            None => None,
        };
        // 1. 检查连接状态
        match &self.client.peer {
            Some(_) => {}
            None => {
                return Err(Box::new(ToolCallError::NotConnected {
                    server: self.server_name.clone(),
                    status: format!("{:?}", self.client.status),
                }));
            }
        }

        let peer = self.client.peer.as_ref().unwrap();
        let session_id = ctx.session_id.as_deref().ok_or_else(|| {
            Box::new(ToolCallError::CallFailed {
                server: self.server_name.clone(),
                tool: self.tool_name.clone(),
                reason: "MCP tool call requires a trusted session binding".into(),
            }) as Box<dyn std::error::Error + Send + Sync>
        })?;
        let pool = self
            .output_pool
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| {
                Box::new(ToolCallError::CallFailed {
                    server: self.server_name.clone(),
                    tool: self.tool_name.clone(),
                    reason: "session MCP task owner unavailable".into(),
                }) as Box<dyn std::error::Error + Send + Sync>
            })?;
        let mut arguments = input.as_object().cloned().unwrap_or_default();
        if self.tool_name == "TaskOutput" {
            let public_id = arguments
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .ok_or("TaskOutput requires a public task_id")?;
            let raw_id =
                pool.resolve_external_task_route(session_id, public_id, &self.server_name)?;
            arguments.insert("task_id".into(), raw_id.into());
        }
        let mut execution_guard = pool
            .begin_external_task_execution(session_id, &self.server_name)
            .map_err(|reason| {
                Box::new(ToolCallError::CallFailed {
                    server: self.server_name.clone(),
                    tool: self.tool_name.clone(),
                    reason,
                }) as Box<dyn std::error::Error + Send + Sync>
            })?;

        let delivery = ctx.task_terminal_delivery.clone();
        // 2. 构建 rmcp 请求参数
        let mut request = rmcp::model::CallToolRequestParams::new(self.tool_name.clone())
            .with_arguments(arguments);
        // per-call 会话身份（`peri/taskOwner`）随源接线：只有 Workspace 实例
        // 按直接发起会话隔离后台任务，其他 server 不注入 Peri 元数据。
        if let Some(owner) = super::client::task_owner_metadata::task_owner_meta(
            self.client.source.as_ref(),
            session_id,
        ) {
            request.meta = Some(owner);
        }
        request.meta = Some(super::invocation::request_meta(&ctx, request.meta)?);

        // Workspace tools retain their own deadlines (Bash promotes at <=120s).
        // Source identity, not a spoofable server name, grants this behavior.
        let timeout = (!matches!(
            self.client.source.as_ref(),
            Some(super::config::ConfigSource::Builtin { instance }) if instance == "workspace"
        ))
        .then_some(TOOL_CALL_TIMEOUT);
        let response = match super::tool_request::call_tool(peer, request, timeout).await {
            Ok(response) => response,
            Err(error) => {
                // 只有真正的传输/协议失败走这条路。远端 JSON-RPC error 的
                // `code`/`message`/`data` 目前只经 `Display` 进入文本投影；
                // 移除条件（阶段 2/3）：App relay 需要把上游 JSON-RPC error 转发给 App 时
                // （需要 App 侧消费方 + Host invoke 入口共用同一出口），在此用
                // `rmcp::ServiceError::McpError(data)` 构造 `RemoteToolError::from_jsonrpc`
                // 并挂到 `EffectiveToolError::with_remote_error` 上，不靠 error code 反推。
                let e = error;
                return Err(Box::new(
                    if let rmcp::ServiceError::Timeout { timeout } = e {
                        ToolCallError::Timeout {
                            server: self.server_name.clone(),
                            tool: self.tool_name.clone(),
                            timeout_secs: timeout.as_secs(),
                        }
                    } else {
                        ToolCallError::CallFailed {
                            server: self.server_name.clone(),
                            tool: self.tool_name.clone(),
                            reason: e.to_string(),
                        }
                    },
                ));
            }
        };
        let result = match response {
            rmcp::model::CallToolResponse::Complete(result) => {
                execution_guard.confirm_stopped();
                // 未协商 Tasks 能力的客户端拿到的是 Workspace 内部任务 id；为
                // TaskOutput 记录一条 raw→raw 的会话内可信路由，避免前缀截取。
                if self.tool_name == "Bash" {
                    if let Some(task_id) = running_task_id(&result) {
                        if let Err(error) = pool.record_external_task_route(
                            session_id,
                            &task_id,
                            &self.server_name,
                            &task_id,
                        ) {
                            tracing::warn!(
                                %error,
                                server = %self.server_name,
                                "failed to record TaskOutput route for running shell task"
                            );
                        }
                    }
                }
                result
            }
            rmcp::model::CallToolResponse::Task(created) => {
                let task_created_at = created.task.created_at;
                let task_id = created.task.task_id;
                let is_shell = self.tool_name == "Bash"
                    && (matches!(
                        self.client.source.as_ref(),
                        Some(super::config::ConfigSource::Builtin { instance })
                            if instance == "workspace"
                    ) || matches!(
                        self.client.source.as_ref(),
                        Some(super::config::ConfigSource::WorkspaceRemote)
                    ));
                let summary = if is_shell {
                    input
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Bash")
                } else {
                    self.tool_name.as_str()
                };
                let kind = if is_shell {
                    peri_acp_types::tasks::BgTaskKind::Shell
                } else {
                    peri_acp_types::tasks::BgTaskKind::Mcp
                };
                let (manager, registration) = pool.external_task_registration(
                    session_id,
                    Some(session_id),
                    delivery.clone(),
                    &self.server_name,
                    &task_id,
                    kind,
                    summary,
                    is_shell,
                    &task_created_at,
                )?;
                let public_id = match manager.register_external(registration) {
                    Ok(task_id) => {
                        execution_guard.confirm_stopped();
                        task_id
                    }
                    Err(reason) => {
                        return Err(Box::new(ToolCallError::CallFailed {
                            server: self.server_name.clone(),
                            tool: self.tool_name.clone(),
                            reason: format!("{reason}; remote task started but local registration failed; do not repeat the invocation"),
                        }));
                    }
                };
                pool.record_external_task_route(
                    session_id,
                    &public_id,
                    &self.server_name,
                    &task_id,
                )?;
                if let Err(error) = pool.spawn_managed_task_subscription(
                    self.server_name.clone(),
                    session_id.to_owned(),
                    task_id.clone(),
                    public_id.clone(),
                    is_shell,
                    peer.clone(),
                ) {
                    if error != super::task_scope::TaskAdmissionError::DuplicateKey {
                        return Err(Box::new(ToolCallError::CallFailed {
                                    server: self.server_name.clone(),
                                    tool: self.tool_name.clone(),
                                    reason: format!(
                                        "background task {public_id} exists, but its monitor was not admitted: {error}; completion delivery is not guaranteed; do not repeat the command"
                                    ),
                                }));
                    }
                }
                // 回执只承诺可达的投递：有 canonical 路由 = 结果到达时持久记录；
                // 否则只声称活跃期送达。结果尚未产生，不得提前声称已写入 transcript，
                // 也不承诺进程重启后恢复后台执行。
                let delivery_note = if delivery.is_some() {
                    "Its completion result will be delivered to the initiating session when it \
                     arrives; the delivered result is recorded at that point."
                } else {
                    "Its completion result will be delivered to the initiating session \
                     while that session is live."
                };
                return Ok(super::tool_execution::task_started(
                    public_id,
                    delivery_note,
                ));
            }
            _ => {
                return Err(Box::new(ToolCallError::CallFailed {
                    server: self.server_name.clone(),
                    tool: self.tool_name.clone(),
                    reason: "unsupported MCP tool response".into(),
                }));
            }
        };

        // 4. 先保存结果事实：`isError: true` 与多模态 content / structuredContent /
        //    `_meta` 一样属于结果，不再被折叠成错误路径。文本投影是最后一步的派生
        //    视图，不是事实源（step 6）。
        //
        //    保真边界：本值来自 rmcp 解析后的 typed 结果，rmcp `CallToolResult` 没有
        //    未知字段承接位，因此线上未被 rmcp 建模的顶层扩展字段在此已丢失；要覆盖
        //    它们需要在 transport 层抓取原始 JSON（见本函数末尾的移除条件）。
        let raw = RawToolResult::new(serde_json::to_value(&result).map_err(|error| {
            Box::new(ToolCallError::CallFailed {
                server: self.server_name.clone(),
                tool: self.tool_name.clone(),
                reason: format!("failed to serialize MCP tool result: {error}"),
            }) as Box<dyn std::error::Error + Send + Sync>
        })?);

        // 5. 实例化调用（模型 / 嵌套 / Host 发起）为带 UI URI 的工具签发**调用记录**。
        //
        //    App 自己发起的执行不签发记录：来源判定只看 typed
        //    [`InvocationOrigin::App`]（取代旧的 `invocation_id.starts_with("mcp-app:")`
        //    字符串前缀判定）。
        //
        //    签发面（阶段 4a，裁决 D5 + 设计 §4）：
        //    - **`isError:true` 同样签发**：它是结果事实，不是「没有可展示内容」；
        //      模型的失败调用也可能带 UI（原 `!raw.is_error()` 门已删除）。
        //    - 模型可见但 app 不可见的工具同样签发，资格为
        //      [`AppQualification::DisplayOnly`]（可显示、**零 callback 资格**）。
        //
        //    记录是**纯数据** + 五层取消令牌中的 App 层：不再携带 dispatcher、
        //    取消令牌、turn generation、catalog 或 EventBus（设计 §4 删除清单 3）。
        //    App 回调的执行由 session-owned canonical 入口按 record 身份重新取用。
        if let (Some(registry), Some(session_id), Some(invocation_id)) = (
            self.binding_leases.as_ref(),
            ctx.session_id.clone(),
            ctx.invocation_id.clone(),
        ) {
            let app_initiated = ctx
                .origin
                .as_ref()
                .is_some_and(peri_agent::tools::InvocationOrigin::is_app);
            if !app_initiated {
                // 展示资格：带 UI URI，且至少对模型或 App 一面可见（两面都不可见的
                // 工具不得因带 UI URI 而获得展示面）。
                let ui_tool = self
                    .client
                    .tools
                    .iter()
                    .find(|tool| tool.name.as_ref() == self.tool_name)
                    .and_then(|tool| {
                        let visibility = super::apps::tool_visibility(tool);
                        if !visibility.model && !visibility.app {
                            return None;
                        }
                        super::apps::tool_resource_uri(tool)
                            .map(|resource_uri| (resource_uri, visibility))
                    });
                if let Some((resource_uri, visibility)) = ui_tool {
                    let qualification = if visibility.app {
                        let allowed_tools = match ctx.effective_tool_dispatcher.as_ref() {
                            Some(dispatcher) => app_allowed_tools(
                                &self.server_name,
                                &resource_uri,
                                &self.client.tools,
                                dispatcher.as_ref(),
                            ),
                            None => std::collections::HashMap::new(),
                        };
                        // 白名单为空 ⇒ 没有任何可回调工具，等价于仅可显示。
                        if allowed_tools.is_empty() {
                            peri_acp_types::mcp_apps_facade::AppQualification::DisplayOnly
                        } else {
                            peri_acp_types::mcp_apps_facade::AppQualification::Interactive {
                                allowed_tools,
                            }
                        }
                    } else {
                        peri_acp_types::mcp_apps_facade::AppQualification::DisplayOnly
                    };
                    match registry.session_incarnation(&session_id) {
                        Some(incarnation) => {
                            // 实例化令牌：本次执行身份的一次性消费键
                            //（`peri/mcp/open` 用它换取 App 实例）。
                            let invocation_token = invocation_id.to_string();
                            let record =
                                super::apps::McpAppRecord::new(super::apps::McpAppRecordSpec {
                                    owner_session_id: session_id.clone(),
                                    server_id: self.server_name.clone(),
                                    server_generation: self.server_generation,
                                    resource_uri,
                                    instantiating_tool: self.tool_name.clone(),
                                    invocation_id,
                                    invocation_token,
                                    tool_call_id: ctx.tool_call_id.clone(),
                                    incarnation,
                                    qualification,
                                    // 实际（审批后 effective）参数：snapshot 的初始输入真相。
                                    effective_input: input.as_object().cloned().unwrap_or_default(),
                                    // `Arc` 共享：多 tab attach 不复制同一结果（设计 §9.1）。
                                    initial_result: std::sync::Arc::new(raw.clone()),
                                });
                            if let Err(rejection) = registry.issue(std::sync::Arc::new(record)) {
                                tracing::warn!(
                                    server = %self.server_name,
                                    tool = %self.tool_name,
                                    %rejection,
                                    "MCP App record not issued"
                                );
                            }
                        }
                        None => tracing::warn!(
                            server = %self.server_name,
                            tool = %self.tool_name,
                            session_id = %session_id,
                            "MCP App record not issued: session incarnation is not registered"
                        ),
                    }
                }
            }
        }

        // 6. 文本投影（截断超大输出）。投影与改动前逐字节一致：`isError` 结果仍带
        //    失败上下文，模型/ACP 看到的文本不回退，事实由 outcome 承载。
        let formatted = format_contents(&result.content);
        let pool = self.output_pool.as_ref().and_then(std::sync::Weak::upgrade);
        let session_id = self
            .output_session_id
            .as_deref()
            .or(ctx.session_id.as_deref());
        let text = if raw.is_error() {
            let reason = format_output(pool.as_deref(), session_id, formatted, true).await;
            ToolCallError::CallFailed {
                server: self.server_name.clone(),
                tool: self.tool_name.clone(),
                reason,
            }
            .to_string()
        } else {
            format_output(pool.as_deref(), session_id, formatted, false).await
        };
        // 远端 JSON-RPC error 的原始载荷尚未在协议失败分支接线，见该分支注释。
        super::tool_execution::complete(self, text, raw)
    }
}

/// 将 content 列表格式化为纯文本字符串（模型面与宿主面 `call_instance_tool` 共用）。
pub(crate) fn format_contents(contents: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for content in contents {
        match content {
            rmcp::model::ContentBlock::Text(text_content) => {
                parts.push(text_content.text.clone());
            }
            rmcp::model::ContentBlock::Image(image_content) => {
                parts.push(format!("[image: {}]", image_content.mime_type));
            }
            rmcp::model::ContentBlock::Resource(embedded) => {
                let uri = match &embedded.resource {
                    rmcp::model::ResourceContents::TextResourceContents { uri, .. } => uri.clone(),
                    rmcp::model::ResourceContents::BlobResourceContents { uri, .. } => uri.clone(),
                    _ => "unknown".to_string(),
                };
                parts.push(format!("[resource: {}]", uri));
            }
            rmcp::model::ContentBlock::Audio(audio_content) => {
                parts.push(format!("[audio: {}]", audio_content.mime_type));
            }
            rmcp::model::ContentBlock::ResourceLink(link) => {
                parts.push(format!("[resource_link: {}]", link.uri));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

/// 会话可见的 typed bridge 集合（唯一 typed 构造入口）。
///
/// `build_tool_bridges` / [`McpToolBridge::with_system_direct`] 的 typed 版本：调用方
/// 可以在同一批对象上做分类后只装箱一次，
/// 避免同一工具被注册两份。两种 constructor、generation 与 binding leases 行为
/// 与原实现一致。
///
/// `session_id` 为 `None` 表示不过滤（部署面视图）；`Some` 时排除其他会话的
/// ACP 连接（`McpClientPool::is_visible_to_session`），避免会话间工具泄漏。
///
/// **IF-D13 生效点**：注册表中**声明为 direct** 的 builtin 工具在这里直接
/// `.with_system_direct()`（`direct = 声明的 direct || 启动期 system_mcp_tools 提升`，两者由
/// 「声明 direct 集合 == `system_mcp_tools` 集合」的断言锁死，不得冲突）。其余一律
/// 保持 deferred。判定只走 `builtin::is_declared_direct`（未实现 / 未知名恒 false），
/// 不在本文件硬编码任何 `mcp__*` 字面量或实例名。
pub(crate) fn build_typed_tool_bridges_visible_to(
    pool: &Arc<McpClientPool>,
    session_id: Option<&str>,
) -> Vec<McpToolBridge> {
    build_bridges(pool, true, session_id)
}

/// 部署面视图的 typed 版本（无会话归属过滤）。
pub(crate) fn build_typed_tool_bridges(pool: &Arc<McpClientPool>) -> Vec<McpToolBridge> {
    build_bridges(pool, true, None)
}

/// 强制 deferred 的 typed 版本：public [`build_tool_bridges`] 专用。
///
/// 与 [`build_typed_tool_bridges_visible_to`] 的唯一差异是不应用声明 direct，因此
/// public builder 在 builtin 与外部 server 两种输入下的行为都与提取出 typed
/// builder 之前**逐位一致**。
pub(crate) fn build_deferred_tool_bridges(pool: &Arc<McpClientPool>) -> Vec<McpToolBridge> {
    build_bridges(pool, false, None)
}

fn build_bridges(
    pool: &Arc<McpClientPool>,
    apply_declared_direct: bool,
    session_id: Option<&str>,
) -> Vec<McpToolBridge> {
    let mut bridges: Vec<McpToolBridge> = Vec::new();
    let mut clients = pool.get_all_clients_visible_to(session_id);
    clients.sort_by(|left, right| left.name.cmp(&right.name));
    for client in clients {
        let generation = pool.handle_generation(&client);
        for tool in &client.tools {
            let mut bridge = McpToolBridge::new(&client.name, tool, Arc::clone(&client))
                .with_server_generation(generation)
                .with_binding_leases(Arc::clone(&pool.app_binding_leases))
                .with_output_store(pool, session_id);
            if apply_declared_direct
                && matches!(
                    &client.source,
                    Some(super::config::ConfigSource::WorkspaceRemote)
                )
            {
                bridge = bridge.with_workspace_direct();
            } else if apply_declared_direct
                && matches!(&client.source, Some(super::config::ConfigSource::Builtin { instance }) if instance == &client.name)
                && super::builtin::is_declared_direct(&client.name, tool.name.as_ref())
            {
                bridge = bridge.with_system_direct();
            }
            bridges.push(bridge);
        }
    }
    bridges
}

/// 从 McpClientPool 的所有已连接客户端中批量创建 McpToolBridge
///
/// 全部返回值保持 deferred 默认行为（`is_direct() == false`）——包括 builtin 实例的
/// 工具：未类型化的 public builder 不参与 direct 提升（IF-D13），保持既有契约不变。
///
/// 不过滤会话归属（部署面视图）：会话内装配必须走
/// [`build_tool_bridges_visible_to`]，否则会拿到其他会话声明的 ACP 工具。
pub fn build_tool_bridges(pool: &Arc<McpClientPool>) -> Vec<Box<dyn BaseTool>> {
    build_deferred_tool_bridges(pool)
        .into_iter()
        .map(|bridge| Box::new(bridge) as Box<dyn BaseTool>)
        .collect()
}

/// 会话可见的 bridge 集合（[`build_tool_bridges`] 的 ACP 归属过滤版）。
pub fn build_tool_bridges_visible_to(
    pool: &Arc<McpClientPool>,
    session_id: Option<&str>,
) -> Vec<Box<dyn BaseTool>> {
    build_bridges(pool, false, session_id)
        .into_iter()
        .map(|bridge| Box::new(bridge) as Box<dyn BaseTool>)
        .collect()
}
/// 统一工具池组装：内置工具优先去重

#[cfg(test)]
#[path = "tool_bridge_test.rs"]
mod tests;

/// C-INJ-01 focused 回归：typed bridge 的 direct 提升不改变 bridge 身份，
/// 且既有 public `build_tool_bridges` 的 deferred 默认行为不变。
#[cfg(test)]
mod direct_flag_tests {
    use super::*;
    use crate::mcp::client::ClientStatus;

    fn make_tool(tool_name: &str) -> Tool {
        serde_json::from_value(serde_json::json!({
            "name": tool_name,
            "description": "Read a file",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } }
            }
        }))
        .unwrap()
    }

    fn make_handle(server: &str, tools: Vec<Tool>, status: ClientStatus) -> Arc<McpClientHandle> {
        Arc::new(McpClientHandle {
            call_gate: Default::default(),
            name: server.to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: None,
            tools,
            resources: vec![],
            status,
            oauth_status: Default::default(),
            source: None,
            url: None,
            skills_capable: false,
        })
    }

    fn make_builtin_handle(instance: &str) -> Arc<McpClientHandle> {
        Arc::new(McpClientHandle {
            call_gate: Default::default(),
            name: instance.to_string(),
            version: None,
            connected_at: None,
            protocol_version: None,
            cache_version: None,
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: Some(crate::mcp::config::ConfigSource::Builtin {
                instance: instance.to_string(),
            }),
            url: None,
            skills_capable: false,
        })
    }

    /// host-only 发现过滤：bridge 的 `visible_to_model()` 等于注册表逐工具
    /// `model_visible`（builtin 来源）；非 builtin 来源不受声明表影响。
    ///
    /// 派生式断言：首个 host-only 条目（workflow 的 `WorkflowAgentResult`）随实例
    /// 注册落地后自动被覆盖，不需要改本用例。
    #[test]
    fn builtin_declared_model_visibility_is_applied_to_bridges() {
        for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
            for tool in instance.tools {
                let bridge = McpToolBridge::new(
                    instance.name,
                    &make_tool(tool.original_name),
                    make_builtin_handle(instance.name),
                );
                assert_eq!(
                    bridge.visible_to_model(),
                    tool.model_visible,
                    "{}/{} 的桥接可见性必须等于注册表 model_visible",
                    instance.name,
                    tool.original_name
                );
            }
        }
        // 非 builtin 来源（source: None）：同名工具名不受 builtin 声明表影响。
        let external = McpToolBridge::new(
            "workflow",
            &make_tool("WorkflowAgentResult"),
            make_handle("workflow", vec![], ClientStatus::Connected),
        );
        assert!(
            external.visible_to_model(),
            "非 builtin 来源不得消费 builtin 声明表"
        );
    }

    #[test]
    fn test_system_direct_flag_preserves_bridge_identity() {
        let bridge = McpToolBridge::new(
            "workspace",
            &make_tool("Read"),
            make_handle("workspace", vec![], ClientStatus::Disconnected),
        );
        assert!(!bridge.is_direct(), "new 缺省必须为 deferred");
        let name = bridge.name().to_string();
        let parameters = bridge.parameters();
        assert!(bridge.visible_to_model());

        let promoted = bridge.with_system_direct();
        assert!(promoted.is_direct());
        assert_eq!(promoted.name(), "Read");
        assert_ne!(promoted.name(), name);
        assert_eq!(promoted.original_tool_name(), "Read");
        assert_eq!(promoted.mcp_server_name(), Some("workspace"));
        assert_eq!(promoted.parameters(), parameters);
        assert!(
            promoted.visible_to_model(),
            "direct 不等于绕过 model visibility"
        );
    }

    /// typed builder 的输入取注册表中**声明 deferred** 的 builtin 工具。
    ///
    /// 只有输入确实声明为 deferred，「不提升」才是有信息的断言：wave 3 起 `workspace`
    /// 的 7 个工具全部 `direct: true`（注册表第五项），继续拿它当输入会变成「提升」的
    /// 反面 —— 断言会红，而修法绝不是放宽断言。名字从注册表派生，不新增第二份名单。
    #[test]
    fn test_build_tool_bridges_keeps_deferred_default_and_matches_typed() {
        let (instance, tool) = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
            .iter()
            .find_map(|instance| {
                instance
                    .tools
                    .iter()
                    .find(|tool| !tool.direct)
                    .map(|tool| (instance, tool))
            })
            .expect("注册表必须至少保留一个 deferred 声明的 builtin 工具");
        let pool = Arc::new(McpClientPool::new_pending());
        let handle = make_handle(
            instance.name,
            vec![make_tool(tool.original_name)],
            ClientStatus::Connected,
        );
        pool.clients
            .write()
            .insert(instance.name.to_string(), Arc::clone(&handle));

        let typed = build_typed_tool_bridges(&pool);
        let boxed = build_tool_bridges(&pool);
        assert_eq!(typed.len(), 1);
        assert_eq!(boxed.len(), typed.len());
        assert_eq!(boxed[0].name(), typed[0].name());
        assert!(
            !typed[0].is_direct(),
            "注册表声明 deferred 的 builtin 工具（{}）在 typed builder 中必须保持 deferred",
            tool.effective_name
        );
        assert!(
            !boxed[0].is_direct(),
            "public builder 必须保持 deferred 默认"
        );
        // 既有 generation / binding leases 传递行为不得因提取 typed builder 而丢失
        assert_eq!(typed[0].server_generation, pool.handle_generation(&handle));
        assert!(typed[0].binding_leases.is_some());
    }

    #[test]
    fn remote_workspace_direct_tools_follow_live_list_without_registry_names() {
        let pool = Arc::new(McpClientPool::new_pending());
        let mut handle = make_handle(
            "workspace",
            vec![make_tool("NewRemoteTool")],
            ClientStatus::Connected,
        );
        Arc::get_mut(&mut handle).unwrap().source =
            Some(super::super::config::ConfigSource::WorkspaceRemote);
        pool.clients.write().insert("workspace".into(), handle);
        let typed = build_typed_tool_bridges(&pool);
        assert_eq!(typed.len(), 1);
        assert_eq!(typed[0].name(), "NewRemoteTool");
        assert!(typed[0].is_direct());
        assert_eq!(typed[0].builtin_mcp_instance(), None);

        let empty = Arc::new(McpClientPool::new_pending());
        let mut handle = make_handle("workspace", vec![], ClientStatus::Connected);
        Arc::get_mut(&mut handle).unwrap().source =
            Some(super::super::config::ConfigSource::WorkspaceRemote);
        empty.clients.write().insert("workspace".into(), handle);
        assert!(build_typed_tool_bridges(&empty).is_empty());
    }
}
