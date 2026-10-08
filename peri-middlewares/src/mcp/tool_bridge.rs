use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
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

fn completed_application_error(
    result: &rmcp::model::CallToolResult,
) -> Option<peri_acp_types::tools::EffectiveToolError> {
    (result.is_error == Some(true)).then(|| {
        peri_acp_types::tools::EffectiveToolError::new(
            peri_acp_types::tools::EffectiveToolErrorCode::ApplicationFailed,
            format_contents(&result.content),
        )
    })
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
    binding_leases: Option<Arc<super::apps::McpAppBindingLeaseRegistry>>,
    admission: Option<super::dynamic::admission::DynamicMcpAdmissionGate>,
    output_pool: Option<std::sync::Weak<McpClientPool>>,
    output_session_id: Option<String>,
}

/// External MCP requests share a 120-second deadline across send and response.
/// Only the workspace builtin retains its native file/shell deadlines. Identity
/// comes from ConfigSource; other builtins retain the bounded MCP call contract.
/// In particular Bash must finish promotion and return its receipt at the 120s boundary.
pub(crate) const TOOL_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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

impl McpToolBridge {
    pub fn new(server_name: &str, tool: &Tool, client: Arc<McpClientHandle>) -> Self {
        let tool_name = tool.name.to_string();
        let full_name = effective_mcp_tool_name(server_name, &tool_name);
        let description =
            bounded_tool_description(server_name, tool.description.as_ref().map(|d| d.as_ref()));
        let input_schema = serde_json::to_value(&*tool.input_schema)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        Self {
            server_name: server_name.to_string(),
            tool_name,
            full_name,
            description,
            input_schema,
            model_visible: super::apps::tool_visibility(tool).model,
            direct: false,
            server_generation: 0,
            client,
            binding_leases: None,
            admission: None,
            output_pool: None,
            output_session_id: None,
        }
    }

    pub fn new_dynamic(
        server_name: &str,
        tool: &Tool,
        client: Arc<McpClientHandle>,
        admission: super::dynamic::admission::DynamicMcpAdmissionGate,
    ) -> Result<Self, ToolCallError> {
        let tool_name = tool.name.to_string();
        if !valid_name_component(server_name) || !valid_name_component(&tool_name) {
            return Err(ToolCallError::Unavailable {
                server: server_name.to_string(),
            });
        }
        let description = bounded_tool_description(
            server_name,
            tool.description.as_ref().map(|value| value.as_ref()),
        );
        Ok(Self {
            server_name: server_name.to_string(),
            full_name: effective_mcp_tool_name(server_name, &tool_name),
            tool_name,
            description,
            input_schema: serde_json::to_value(&*tool.input_schema)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
            model_visible: super::apps::tool_visibility(tool).model,
            direct: false,
            server_generation: 0,
            client,
            binding_leases: None,
            admission: Some(admission),
            output_pool: None,
            output_session_id: None,
        })
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
        registry: Arc<super::apps::McpAppBindingLeaseRegistry>,
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
    pub(crate) fn with_workspace_direct(mut self) -> Self {
        self = self.with_system_direct();
        self.model_visible = true;
        self
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
}

fn valid_name_component(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
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

    async fn invocation_target(
        &self,
        session_id: &str,
        session_lifecycle: u64,
    ) -> Result<Option<peri_acp_types::tools::InvocationTargetMetadata>, String> {
        use sha2::{Digest, Sha256};
        let pool = self
            .output_pool
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .ok_or("Blocked: MCP lifecycle owner unavailable")?;
        let resources = pool
            .session_bindings
            .read()
            .resources(session_id, session_lifecycle)
            .ok_or("Blocked: exact MCP lifecycle resources unavailable")?;
        let snapshot = resources
            .load_session_work(&peri_acp_types::session_resources::work::WorkQuery {
                session_id: session_id.into(),
                limit: 1,
            })
            .await
            .map_err(|error| error.to_string())?;
        let builtin = matches!(
            self.client.source.as_ref(),
            Some(super::config::ConfigSource::Builtin { .. })
        );
        let (canonical, authorization_ref) = if builtin {
            let owners = snapshot
                .state
                .resource_owners
                .get(&session_lifecycle)
                .ok_or("Blocked: durable trusted owner authorization missing")?;
            if owners.authorization_ref.is_empty() {
                return Err("Blocked: durable owner authorization missing".into());
            }
            (
                self.server_name.as_bytes().to_vec(),
                owners.authorization_ref.clone(),
            )
        } else {
            let (configuration, authorization_ref) =
                super::owner_capabilities::trusted_owner_configuration(
                    &snapshot,
                    session_lifecycle,
                    &self.server_name,
                )?;
            (
                serde_json::to_vec(&configuration).map_err(|error| error.to_string())?,
                authorization_ref,
            )
        };
        let workspace = matches!(self.client.source.as_ref(), Some(super::config::ConfigSource::Builtin { instance }) if instance == "workspace")
            || matches!(
                self.client.source.as_ref(),
                Some(super::config::ConfigSource::WorkspaceRemote)
            );
        let (owner_identity, scope_epoch) = if workspace {
            let peer = self
                .client
                .peer
                .as_ref()
                .ok_or("Blocked: MCP owner disconnected")?;
            let meta = pool
                .task_scope_meta_for(&self.server_name, session_id)
                .ok_or("Blocked: trusted MCP scope unavailable")?;
            let response = peri_time::timeout(
                std::time::Duration::from_secs(10),
                peer.send_request(rmcp::model::ClientRequest::CustomRequest(
                    rmcp::model::CustomRequest::new(
                        "workspace/taskCapabilities",
                        Some(serde_json::json!({"_meta": meta})),
                    ),
                )),
            )
            .await
            .map_err(|_| "Blocked: owner discovery timeout")?
            .map_err(|error| format!("Blocked: owner discovery failed: {error}"))?;
            let rmcp::model::ServerResult::CustomResult(result) = response else {
                return Err("Blocked: invalid owner capability response".into());
            };
            let capabilities: super::owner_capabilities::OwnerCapabilities =
                serde_json::from_value(result.0)
                    .map_err(|error| format!("Blocked: invalid owner capabilities: {error}"))?;
            capabilities.validate_scope(session_id)?;
            capabilities.require_close_barrier()?;
            tracing::debug!(server = %self.server_name, recovery = ?capabilities.recovery_guarantee(),
                idempotent = capabilities.invocation_idempotency, "verified owner recovery capability");
            (capabilities.owner_identity, Some(capabilities.scope_epoch))
        } else {
            (format!("best-effort:{:x}", Sha256::digest(canonical)), None)
        };
        Ok(Some(peri_acp_types::tools::InvocationTargetMetadata {
            owner_identity,
            scope_id: session_id.into(),
            scope_epoch,
            authorization_ref,
            recovery_locator: self.server_name.clone(),
        }))
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
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
        let owner_identity = ctx
            .invocation_intent
            .as_ref()
            .ok_or(super::invocation::InvocationError::MissingIdentity)?
            .owner_identity
            .clone();
        let invocation = super::invocation::McpInvocation::from_context(
            &ctx,
            &input,
            &self.full_name,
            &owner_identity,
        )?;
        invocation.prepare().await?;

        // 2. 构建 rmcp 请求参数
        let arguments = input.as_object().cloned().unwrap_or_default();
        let mut request = rmcp::model::CallToolRequestParams::new(self.tool_name.clone())
            .with_arguments(arguments);
        if matches!(self.client.source.as_ref(), Some(super::config::ConfigSource::Builtin { instance }) if instance == "workspace")
            || matches!(
                self.client.source.as_ref(),
                Some(super::config::ConfigSource::WorkspaceRemote)
            )
        {
            request.meta = pool.task_scope_meta_for(&self.server_name, session_id);
        }
        request.meta = Some(invocation.request_meta(request.meta)?);

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
                invocation.record_outcome_unknown().await?;
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
                result
            }
            rmcp::model::CallToolResponse::Task(created) => {
                let task_created_at = created.task.created_at;
                let task_id = created.task.task_id;
                let binding = match invocation.bind_task(&task_id).await {
                    Ok(binding) => binding,
                    Err(error) => {
                        invocation.record_outcome_unknown().await?;
                        return Err(Box::new(error));
                    }
                };
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
                let (manager, registration) = pool.external_task_registration_for_lifecycle(
                    session_id,
                    binding.recipient_lifecycle,
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
                        invocation.record_outcome_unknown().await?;
                        return Err(Box::new(ToolCallError::CallFailed {
                            server: self.server_name.clone(),
                            tool: self.tool_name.clone(),
                            reason: format!("{reason}; immutable owner task {task_id} retained for reconciliation; do not repeat the invocation"),
                        }));
                    }
                };
                if let Err(error) = pool.spawn_managed_task_subscription_for_lifecycle(
                    self.server_name.clone(),
                    session_id.to_owned(),
                    task_id.clone(),
                    public_id.clone(),
                    is_shell,
                    peer.clone(),
                    binding.recipient_lifecycle,
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
                // 回执只承诺可达的投递：有 canonical 路由 = 持久送达
                // （resume 后仍可见）；否则只声称活跃期送达。
                let delivery_note = if delivery.is_some() {
                    "Its completion reminder is committed to the initiating session's \
                             transcript and stays visible after resume."
                } else {
                    "Its completion reminder is delivered to the initiating session \
                             while that session is live."
                };
                let recovery_note = if ctx
                    .invocation_intent
                    .as_ref()
                    .is_some_and(|intent| intent.scope_epoch.is_none())
                {
                    " Recovery is best-effort at-least-once; a lost owner response is OutcomeUnknown, not proof of success."
                } else {
                    " Recovery discovery is available only while this resource owner remains alive."
                };
                return Ok(format!(
                    "Background task started: {public_id}. {delivery_note}{recovery_note}"
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

        // 4. 处理 is_error 标志。失败的实例化调用不得签发 App lease。
        if let Some(application_error) = completed_application_error(&result) {
            let pool = self.output_pool.as_ref().and_then(std::sync::Weak::upgrade);
            let reason = format_output(
                pool.as_deref(),
                self.output_session_id
                    .as_deref()
                    .or(ctx.session_id.as_deref()),
                application_error.message,
                true,
            )
            .await;
            return Err(Box::new(peri_acp_types::tools::EffectiveToolError::new(
                application_error.code,
                ToolCallError::CallFailed {
                    server: self.server_name.clone(),
                    tool: self.tool_name.clone(),
                    reason,
                }
                .to_string(),
            )));
        }

        if let (
            Some(registry),
            Some(dispatcher),
            Some(session_id),
            Some(turn_generation),
            Some(invocation_id),
        ) = (
            self.binding_leases.as_ref(),
            ctx.effective_tool_dispatcher.clone(),
            ctx.session_id.clone(),
            ctx.turn_generation.clone(),
            ctx.invocation_id.clone(),
        ) {
            if invocation_id.starts_with("mcp-app:") {
                if let Ok(raw_result) = serde_json::to_value(&result)
                    .and_then(serde_json::from_value::<peri_acp_types::mcp_apps::RawCallToolResult>)
                {
                    registry.record_raw_result(
                        &invocation_id,
                        serde_json::to_value(raw_result).unwrap_or(serde_json::Value::Null),
                    );
                }
            } else if let Some(resource_uri) = self
                .client
                .tools
                .iter()
                .find(|tool| tool.name.as_ref() == self.tool_name)
                .filter(|tool| super::apps::tool_visibility(tool).app)
                .and_then(super::apps::tool_resource_uri)
            {
                let allowed_tools = app_allowed_tools(
                    &self.server_name,
                    &resource_uri,
                    &self.client.tools,
                    dispatcher.as_ref(),
                );
                registry.issue(super::apps::McpAppBindingLease::new(
                    session_id,
                    turn_generation,
                    self.server_name.clone(),
                    self.server_generation,
                    resource_uri,
                    self.tool_name.clone(),
                    invocation_id,
                    allowed_tools,
                    dispatcher,
                    ctx.cancellation.clone(),
                ));
            }
        }

        // 5. 格式化返回（截断超大输出）
        let formatted = format_contents(&result.content);
        let pool = self.output_pool.as_ref().and_then(std::sync::Weak::upgrade);
        let output = format_output(
            pool.as_deref(),
            self.output_session_id
                .as_deref()
                .or(ctx.session_id.as_deref()),
            formatted,
            false,
        )
        .await;
        Ok(output)
    }
}

/// 将 content 列表格式化为纯文本字符串
fn format_contents(contents: &[ContentBlock]) -> String {
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
            name: server.to_string(),
            version: None,
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
