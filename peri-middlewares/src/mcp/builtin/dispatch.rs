//! builtin handler 的**名字分派**层（owner H-01）。
//!
//! 职责：把实例名（server name / 配置 key / `TransportConfig::Builtin.instance`）解析成
//! 具体的 `ServerHandler`，并以枚举擦除类型差异交给 `mcp::builtin::runtime` 装配。
//! 本模块**只**做分派与转发，不持有实例语义：
//! - 实例的业务面（工具清单、`tools/call` 的结果映射）在独立的 `peri-mcp-*` crates；
//! - 实例所需状态（cron scheduler / cwd / workspace 的 session 级输入）统一经
//!   [`BuiltinInstanceContext`] 传入——`cwd` 不再是独立参数，输入是否齐备由
//!   `context::instance_input_ready` 在 dispatch **之前**判定，因此本工厂的 `None` 只
//!   表示「handler 未接线」，不表示「输入没给全」（`workspace` 例外：它的输入缺失是
//!   「可见但退化」，输入齐备判定不为它增加 arm，见 AW3-11）；
//! - 名字到模块的映射是**代码**事实（模块不能由数据构造）；实例名 / 工具名 / `direct` /
//!   保留名的唯一事实源仍是注册表（`peri_acp_types::builtin_mcp`）。

use std::path::Path;
use std::sync::Arc;

use peri_acp_types::builtin_mcp::find;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, CustomRequest, CustomResult,
        ErrorCode, GetTaskParams, GetTaskResult, ListResourceTemplatesResult, ListResourcesResult,
        ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ServerConfig, SubscriptionFilter, UpdateTaskParams,
    },
    service::{RequestContext, RoleServer, SubscriptionContext},
    ErrorData as McpError, ServerHandler,
};

use super::context::BuiltinInstanceContext;
use peri_mcp_artifact::ArtifactMcpServer;
use peri_mcp_cron::CronMcpServer;
use peri_mcp_web::WebMcpServer;
use peri_mcp_workspace::WorkspaceMcpServer;

/// 实例的 handler 类型擦除：`runtime` 需要在运行时按实例名选择 handler，
/// 而 `rmcp::serve_server` 要求泛型 `S: ServerHandler`（`Arc<dyn ServerHandler>`
/// 不满足该约束），因此用枚举分派。
///
/// 枚举成员与注册表 [`peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES`] 的已实现
/// 实例**逐项对应**：遗漏任一实例都会在装配点落成 typed `HandlerNotWired`，
/// 不会静默回退到别的实例。
pub(crate) enum BuiltinServerHandler {
    Web(WebMcpServer),
    Artifact(ArtifactMcpServer),
    Cron(CronMcpServer),
    Workspace(WorkspaceMcpServer),
}

impl ServerHandler for BuiltinServerHandler {
    // 不覆写 `discover`（§10 R2）。

    fn get_info(&self) -> ServerConfig {
        match self {
            Self::Web(server) => server.get_info(),
            Self::Artifact(server) => server.get_info(),
            Self::Cron(server) => server.get_info(),
            Self::Workspace(server) => server.get_info(),
        }
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        match self {
            Self::Web(server) => server.list_tools(request, context).await,
            Self::Artifact(server) => server.list_tools(request, context).await,
            Self::Cron(server) => server.list_tools(request, context).await,
            Self::Workspace(server) => server.list_tools(request, context).await,
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        match self {
            Self::Web(server) => server.call_tool(request, context).await,
            Self::Artifact(server) => server.call_tool(request, context).await,
            Self::Cron(server) => server.call_tool(request, context).await,
            Self::Workspace(server) => server.call_tool(request, context).await,
        }
    }

    // ── 资源与订阅面（Git Watch 下沉，D-5；W1 资源面扩展）─────────────────────
    //
    // **必须转发**：`resources/*`、`resources/templates/list`、`subscriptions/listen`
    // 与 `skills/*`（custom request）都由 rmcp 按 handler trait 方法分派，枚举不转发
    // 就等于这些能力在 builtin 链路上不存在（客户端会收到 `-32601` / 空 filter）。
    // 当前只有 `workspace` 声明资源、订阅与 skills；其余实例保持各实例自身的既有
    // 退化语义（无资源 ⇒ 默认空表 / `-32601`，无订阅 ⇒ `None`，无 custom ⇒
    // `-32601`），不新造第三种形态。

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        match self {
            Self::Workspace(server) => server.list_resources(request, context).await,
            _ => Ok(ListResourcesResult::default()),
        }
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        match self {
            Self::Workspace(server) => server.list_resource_templates(request, context).await,
            _ => Ok(ListResourceTemplatesResult::default()),
        }
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        match self {
            Self::Workspace(server) => server.read_resource(request, context).await,
            _ => Err(McpError::method_not_found::<
                rmcp::model::ReadResourceRequestMethod,
            >()),
        }
    }

    /// custom request 转发（W1：`skills/list|get` 到 workspace handler）。
    ///
    /// 未覆盖的 method 与未声明 custom 面的实例都落到 trait 的默认语义
    /// （`-32601`）；本枚举不解释 method 语义、不缓存响应。
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        match self {
            Self::Workspace(server) => server.on_custom_request(request, context).await,
            _ => {
                let CustomRequest { method, .. } = request;
                Err(McpError::new(ErrorCode::METHOD_NOT_FOUND, method, None))
            }
        }
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        match self {
            Self::Workspace(server) => server.accepted_subscription_filter(requested),
            _ => None,
        }
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        match self {
            Self::Workspace(server) => server.listen(context).await,
            _ => {
                context.cancelled().await;
                Ok(())
            }
        }
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        match self {
            Self::Workspace(server) => server.get_task(request, context).await,
            _ => Err(McpError::invalid_params("unknown task", None)),
        }
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        match self {
            Self::Workspace(server) => server.update_task(request, context).await,
            _ => Err(McpError::invalid_params("unknown task", None)),
        }
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        match self {
            Self::Workspace(server) => server.cancel_task(request, context).await,
            _ => Err(McpError::invalid_params("unknown task", None)),
        }
    }
}

/// 实例名 → handler 工厂（`mcp::builtin::runtime::spawn_builtin_transport_with_context`
/// 的唯一入口；`None` = 已注册但 handler 未接线，调用方按 `HandlerNotWired` 收口）。
///
/// `runtime.rs` 的接线形如：
///
/// ```ignore
/// let handler = super::dispatch::builtin_server_handler(instance, ctx)
///     .ok_or_else(|| BuiltinSpawnError::HandlerNotWired { instance: instance.to_string() })?;
/// spawn_builtin_transport_with_handler(instance, handler)
/// ```
///
/// 名字到模块的分派是**代码**事实（模块不能由数据构造）；实例名 / 工具名 /
/// `direct` / 保留名的唯一事实源仍是注册表（`peri_acp_types::builtin_mcp`）。
///
/// 各实例的状态来源（全部取自同一 `ctx`，不经 `cwd` 推导）：
/// - `web`：无额外状态；`ctx.cwd` 被忽略；
/// - `artifact`：`ctx.cwd` 是相对路径解析根（不是安全沙箱）；
/// - `cron`：`ctx.cron` 的 scheduler 以 `Arc` 克隆进 handler（A1：组合根同一份，
///   本工厂不新建第二份，也不挂 tick——tick 归 pool 的唯一 spawn 点 A32）；
/// - `workspace`：`ctx.cwd` 是 7 个工具共享的 host cwd（相对路径解析根 + `Bash` 的
///   `current_dir`），`ctx.workspace` 的 session 级输入以 `Clone` 克隆进 handler
///   （`Arc` 克隆，不复制状态：`task_manager` / `on_bg_complete` 各只被搬进 `BashTool`
///   的对应字段）；`ctx.workspace_resources` 的**资源面**输入同样以 `Clone` 转交给
///   `WorkspaceMcpServer::with_resources`（`None` = 资源面未接线）。两个槽位各自独立：
///   资源面不因 session 级输入缺失而消失，反之亦然。
///
/// **与 `cron` 的差别（AW3-11）**：`workspace` 的 arm 是**无条件**构造的——
/// `ctx.workspace` 为 `None` 时同样返回 `Some`（「可见但退化」：实例照常装配，只有 `Bash`
/// 失去后台任务那一路），因此 `None` 在这一支上**不是** `HandlerNotWired`。
///
/// **`None` 的诚实口径**：对 `cron`，缺对应输入的上下文同样返回 `None`
/// （`ctx.cron` 为 `None` 时无状态可注入）。但这条路径经冻结 seam
/// **不可达**：`runtime::spawn_builtin_transport_with_context` 在调本工厂**之前**已用
/// `BuiltinInstanceContext::instance_input_ready` 给出 typed
/// `BuiltinSpawnError::InstanceInputMissing`，因此本工厂的 `None` 只在
/// 「handler 未接线」这一种语义上到达调用方——本工厂**不**、也不能区分
/// 「输入缺失」与「未接线」（`Option` 承载不了两个原因，拆开写会与 seam 的顺序约定
/// 重复判定）。**直接调用本工厂的代码（如 `dispatch` 的测试）必须自行保证输入齐备**；
/// `workspace` 是唯一不适用本条的名字（缺输入仍返回 `Some`）。
pub(crate) fn builtin_server_handler_with_env(
    instance: &str,
    ctx: &BuiltinInstanceContext,
    env: &std::collections::HashMap<String, String>,
) -> Option<BuiltinServerHandler> {
    match find(instance)?.name {
        "web" => Some(BuiltinServerHandler::Web(WebMcpServer::new())),
        "artifact" => Some(BuiltinServerHandler::Artifact(
            ArtifactMcpServer::with_instance_env(Path::new(&ctx.cwd), env),
        )),
        "cron" => ctx.cron.as_ref().map(|cron| {
            BuiltinServerHandler::Cron(CronMcpServer::new(Arc::clone(&cron.scheduler)))
        }),
        "workspace" => {
            // The MCP instance owns its Bash tasks. Session state is never
            // injected into the capability server.
            let mut server = WorkspaceMcpServer::standalone(ctx.cwd.clone());
            if let Some(authority) = ctx.task_scope_authority.get() {
                server = server.with_task_scope_authority(authority.clone());
            }
            // 资源面输入（装配期一次注入的槽位）：`None` = 资源面未接线（既有行为），
            // `Some` = 装载 provider（W4a：会话装配只装 meta 面，见
            // `peri-acp/src/host/workspace.rs` 的构造点）。本工厂不读配置、不派生根。
            let server = match ctx.workspace_resources.as_ref() {
                Some(resources) => server.with_resources(resources.clone()),
                None => server,
            };
            Some(BuiltinServerHandler::Workspace(server))
        }
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn builtin_server_handler(
    instance: &str,
    ctx: &BuiltinInstanceContext,
) -> Option<BuiltinServerHandler> {
    builtin_server_handler_with_env(instance, ctx, &std::collections::HashMap::new())
}

#[cfg(test)]
#[path = "dispatch_test.rs"]
mod tests;
