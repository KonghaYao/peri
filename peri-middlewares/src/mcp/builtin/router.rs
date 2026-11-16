//! v5 builtin 单出口 router（`peri`）：聚合六项能力的 tools/resources/tasks。
//!
//! 运行时边界：router 只做解析、路由、任务归属、错误/panic 隔离与 readiness，
//! 不复制能力业务规则。能力面以 [`BUILTIN_V5_CAPABILITIES`] 为唯一声明源；handler
//! 仍由 `peri-mcp-*` crate 提供（in-process），或由 [`UpstreamProxy`] 代理上游
//! （workspace 的 Sandbox MCP descriptor 路径）。
//!
//! 开关语义：关闭的能力不进入工具、资源、订阅与任务路由；初始化失败的能力只降级
//! 自己；全部失败时 [`RouterTemplate::create`] 返回错误（fail-closed），不启动空出口。

use std::{
    collections::{BTreeSet, HashMap},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
};

use futures::FutureExt;
use peri_acp_types::{
    builtin_mcp::{
        v5::{
            capability as find_capability, capability_of_original_tool, capability_of_resource_uri,
            resolve_model_tool, BuiltinCapability, RoutedTool,
        },
        BUILTIN_V5_CAPABILITIES, BUILTIN_V5_CAPABILITIES_ENV,
    },
    skills::SKILLS_EXTENSION_ID,
};
use peri_mcp_workspace::WorkspaceMcpServer;
use rmcp::{
    model::Implementation,
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, CancelledNotificationParam,
        CreateTaskResult, CustomNotification, CustomRequest, CustomResult, ErrorCode,
        GetSkillRequestParams, GetSkillResult, GetTaskParams, GetTaskResult,
        ListResourceTemplatesResult, ListResourcesResult, ListSkillsResult, ListToolsResult,
        PaginatedRequestParams, ProgressNotificationParam, ReadResourceDirectoryRequestParams,
        ReadResourceDirectoryResult, ReadResourceRequestParams, ReadResourceResponse, Resource,
        ResourcesCapability, ServerCapabilities, ServerConfig, SubscribeRequestParams,
        SubscriptionFilter, ToolsCapability, UnsubscribeRequestParams, UpdateTaskParams,
        TASKS_EXTENSION_ID,
    },
    service::{NotificationContext, RequestContext, RoleServer, SubscriptionContext},
    ErrorData as McpError, ServerHandler,
};
use thiserror::Error;

use self::router_handler::CapabilityHandler;
use super::context::BuiltinInstanceContext;
use super::dispatch::{builtin_server_handler_with_env, BuiltinServerHandler};
use super::standalone::StandaloneFactory;
use crate::mcp::upstream_proxy::UpstreamProxy;

/// 单出口 server 名（v5 保留名）。
const ROUTER_SERVER_NAME: &str = "peri";

/// capability 级构建/装配错误。
#[derive(Debug, Error)]
pub(crate) enum RouterBuildError {
    #[error("unknown capability: {name}")]
    UnknownCapability { name: String },
    #[error("capability `{capability}` requires capability `{dependency}`, which is disabled")]
    DependencyDisabled {
        capability: &'static str,
        dependency: &'static str,
    },
    #[error("no builtin capability is available: {}", degraded.join("; "))]
    NoCapabilities { degraded: Vec<String> },
}

struct RouterEntry {
    declared: &'static BuiltinCapability,
    handler: CapabilityHandler,
}

impl RouterEntry {
    fn status(&self) -> CapabilityStatus {
        self.handler.health()
    }

    fn is_ready(&self) -> bool {
        matches!(self.status(), CapabilityStatus::Ready)
    }

    fn readiness(&self) -> CapabilityReadiness {
        CapabilityReadiness {
            capability: self.declared.name,
            status: self.status(),
            tools: self
                .declared
                .tools
                .iter()
                .filter(|tool| tool.model_visible)
                .count(),
            resources: self.declared.resources.static_resources.len()
                + self.declared.resources.resource_templates.len(),
        }
    }

    fn accepts_resource(&self, uri: &str) -> bool {
        self.declared.resources.static_resources.contains(&uri)
            || self
                .declared
                .resources
                .uri_prefixes
                .iter()
                .any(|prefix| uri.starts_with(prefix))
    }

    fn accepts_subscriptions(&self) -> bool {
        !self.declared.resources.default_subscriptions.is_empty()
    }

    fn supports_tasks(&self) -> bool {
        self.handler.get_info().capabilities.supports_tasks()
    }
}

fn capability_dependencies_ready(
    capability: &'static BuiltinCapability,
    enabled: &BTreeSet<&'static str>,
) -> Result<(), RouterBuildError> {
    for dependency in capability.dependencies {
        if !enabled.contains(dependency) {
            return Err(RouterBuildError::DependencyDisabled {
                capability: capability.name,
                dependency,
            });
        }
    }
    Ok(())
}

/// 声明顺序的启用能力集；未知名字是加载期 typed error。
fn selected_capabilities(
    enabled: &[String],
) -> Result<Vec<&'static BuiltinCapability>, RouterBuildError> {
    let mut requested = BTreeSet::new();
    for name in enabled {
        let capability = find_capability(name)
            .ok_or_else(|| RouterBuildError::UnknownCapability { name: name.clone() })?;
        requested.insert(capability.name);
    }
    if requested.is_empty() {
        return Err(RouterBuildError::NoCapabilities { degraded: vec![] });
    }
    for capability in BUILTIN_V5_CAPABILITIES {
        if requested.contains(capability.name) {
            capability_dependencies_ready(capability, &requested)?;
        }
    }
    Ok(BUILTIN_V5_CAPABILITIES
        .iter()
        .filter(|capability| requested.contains(capability.name))
        .collect())
}

/// 能力装配源：in-process handler 工厂或上游代理。
enum SelectionSource {
    Builtin(Box<StandaloneFactory>),
    Upstream(UpstreamProxy),
    Failed(String),
}

struct Selection {
    declared: &'static BuiltinCapability,
    source: SelectionSource,
}

/// 会话级 router 模板：共享 handler 工厂（workspace/monitor 任务表、cron tick），
/// 每个会话通过 [`Self::create`] 建立独立 router 状态。
pub(crate) struct RouterTemplate {
    selections: Vec<Selection>,
    disabled: Vec<&'static str>,
    workspace: Option<WorkspaceMcpServer>,
}

impl RouterTemplate {
    /// 由能力开关构建 in-process 能力集；单能力初始化失败只降级该能力。
    pub(crate) fn new(enabled: &[String], workspace: PathBuf) -> Result<Self, RouterBuildError> {
        Self::build(enabled, workspace, None)
    }

    /// 宿主路径的能力集：配置标记 ∩ 会话关闭集；缺失标记时退回全部能力。
    pub(crate) fn host_capabilities(
        env: &HashMap<String, String>,
        closed: &BTreeSet<String>,
    ) -> Vec<String> {
        let configured = env
            .get(BUILTIN_V5_CAPABILITIES_ENV)
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| {
                super::enabled_capabilities(closed)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            });
        configured
            .into_iter()
            .filter(|name| {
                find_capability(name)
                    .is_none_or(|capability| !closed.contains(capability.legacy_instance))
            })
            .collect()
    }

    /// workspace 能力改由上游代理提供（Sandbox MCP descriptor 路径）。
    /// 宿主装配接入前只由 router 测试消费。
    #[allow(dead_code)]
    pub(crate) fn with_workspace_upstream(
        enabled: &[String],
        workspace: PathBuf,
        proxy: UpstreamProxy,
    ) -> Result<Self, RouterBuildError> {
        Self::build(enabled, workspace, Some(proxy))
    }

    fn build(
        enabled: &[String],
        workspace: PathBuf,
        workspace_proxy: Option<UpstreamProxy>,
    ) -> Result<Self, RouterBuildError> {
        let selected = selected_capabilities(enabled)?;
        let requested: BTreeSet<&'static str> =
            selected.iter().map(|capability| capability.name).collect();
        let mut selections = Vec::with_capacity(selected.len());
        let mut workspace_handle = None;
        for declared in selected {
            let source = if declared.name == "workspace" {
                match &workspace_proxy {
                    Some(proxy) => SelectionSource::Upstream(proxy.clone()),
                    None => match build_factory(declared, &workspace) {
                        Ok(factory) => {
                            workspace_handle = factory.workspace_handle();
                            SelectionSource::Builtin(Box::new(factory))
                        }
                        Err(reason) => {
                            degrade(declared, &reason);
                            SelectionSource::Failed(reason)
                        }
                    },
                }
            } else {
                match build_factory(declared, &workspace) {
                    Ok(factory) => SelectionSource::Builtin(Box::new(factory)),
                    Err(reason) => {
                        degrade(declared, &reason);
                        SelectionSource::Failed(reason)
                    }
                }
            };
            selections.push(Selection { declared, source });
        }
        let disabled = BUILTIN_V5_CAPABILITIES
            .iter()
            .filter(|capability| !requested.contains(capability.name))
            .map(|capability| capability.name)
            .collect();
        Ok(Self {
            selections,
            disabled,
            workspace: workspace_handle,
        })
    }

    /// 会话 handler；全部能力不可用时 fail-closed。
    pub(crate) fn create(&self) -> Result<BuiltinRouter, RouterBuildError> {
        self.create_with_workspace().map(|(router, _)| router)
    }

    /// 会话 handler 与 workspace shell 任务句柄。
    ///
    /// 宿主 transport 的监督者需要在关闭时收敛 workspace shell 任务；句柄只在
    /// workspace 能力真的装配成功时存在。
    pub(crate) fn create_with_workspace(
        &self,
    ) -> Result<
        (
            BuiltinRouter,
            Option<peri_mcp_workspace::WorkspaceMcpServer>,
        ),
        RouterBuildError,
    > {
        let mut entries = Vec::new();
        let mut degraded: Vec<(&'static str, String)> = Vec::new();
        let workspace_handle = self.workspace.clone();
        for selection in &self.selections {
            match &selection.source {
                SelectionSource::Builtin(factory) => match create_handler(factory) {
                    Ok(handler) => entries.push(RouterEntry {
                        declared: selection.declared,
                        handler: CapabilityHandler::InProcess(Box::new(handler)),
                    }),
                    Err(reason) => {
                        degrade(selection.declared, &reason);
                        degraded.push((selection.declared.name, reason));
                    }
                },
                SelectionSource::Upstream(proxy) => entries.push(RouterEntry {
                    declared: selection.declared,
                    handler: CapabilityHandler::Upstream(proxy.clone()),
                }),
                SelectionSource::Failed(reason) => {
                    degraded.push((selection.declared.name, reason.clone()));
                }
            }
        }
        let degraded_reasons: Vec<String> = degraded
            .iter()
            .map(|(name, reason)| format!("{name}: {reason}"))
            .collect();
        if entries.len() + degraded.len() != self.selections.len() {
            // 不可达：每个 selection 恰好落入 entries 或 degraded。
            return Err(RouterBuildError::NoCapabilities {
                degraded: degraded_reasons,
            });
        }
        if !entries.iter().any(RouterEntry::is_ready) {
            return Err(RouterBuildError::NoCapabilities {
                degraded: degraded_reasons,
            });
        }
        if !degraded.is_empty() {
            tracing::warn!(
                degraded = %degraded_reasons.join(", "),
                "builtin capability router started with degraded capabilities"
            );
        }
        Ok((
            BuiltinRouter {
                entries,
                disabled: self.disabled.clone(),
                degraded,
                tasks: TaskRoutes::default(),
            },
            workspace_handle,
        ))
    }

    /// workspace shell 任务清理句柄（跨会话共享）。
    pub(crate) fn workspace_handle(&self) -> Option<WorkspaceMcpServer> {
        self.workspace.clone()
    }
}

/// 宿主会话的单出口装配：从已注入的实例上下文构造每个能力的 in-process handler。
///
/// 与 standalone 模板的区别：不需要跨连接复用工厂；每次 transport spawn 构造一套
/// handler（与迁移期逐实例 spawn 的会话语义一致）。单能力构造失败只降级自己，
/// 全部失败返回 [`RouterBuildError::NoCapabilities`]（fail-closed）。
pub(crate) fn build_host_router(
    context: &BuiltinInstanceContext,
    env: &HashMap<String, String>,
) -> Result<
    (
        BuiltinRouter,
        Option<peri_mcp_workspace::WorkspaceMcpServer>,
    ),
    RouterBuildError,
> {
    let enabled = RouterTemplate::host_capabilities(env, &context.closed);
    let selected = selected_capabilities(&enabled)?;
    let requested: BTreeSet<&'static str> =
        selected.iter().map(|capability| capability.name).collect();
    let mut entries = Vec::new();
    let mut degraded: Vec<(&'static str, String)> = Vec::new();
    let mut workspace_handle = None;
    for declared in selected {
        let instance = declared.legacy_instance;
        match catch_build(declared.name, || {
            builtin_server_handler_with_env(instance, context, env)
                .ok_or_else(|| format!("handler not wired for capability {instance}"))
        }) {
            Ok(handler) => {
                if let BuiltinServerHandler::Workspace(server) = &handler {
                    workspace_handle = Some(server.clone());
                }
                entries.push(RouterEntry {
                    declared,
                    handler: CapabilityHandler::InProcess(Box::new(handler)),
                });
            }
            Err(reason) => {
                degrade(declared, &reason);
                degraded.push((declared.name, reason));
            }
        }
    }
    let degraded_reasons: Vec<String> = degraded
        .iter()
        .map(|(name, reason)| format!("{name}: {reason}"))
        .collect();
    if !entries.iter().any(RouterEntry::is_ready) {
        return Err(RouterBuildError::NoCapabilities {
            degraded: degraded_reasons,
        });
    }
    if !degraded.is_empty() {
        tracing::warn!(
            degraded = %degraded_reasons.join(", "),
            "builtin capability router started with degraded capabilities"
        );
    }
    let disabled = BUILTIN_V5_CAPABILITIES
        .iter()
        .filter(|capability| !requested.contains(capability.name))
        .map(|capability| capability.name)
        .collect();
    Ok((
        BuiltinRouter {
            entries,
            disabled,
            degraded,
            tasks: TaskRoutes::default(),
        },
        workspace_handle,
    ))
}

fn build_factory(
    declared: &'static BuiltinCapability,
    workspace: &Path,
) -> Result<StandaloneFactory, String> {
    catch_build(declared.name, || {
        StandaloneFactory::try_new(declared.legacy_instance, workspace.to_path_buf())
            .map_err(|error| error.to_string())
    })
}

fn create_handler(factory: &StandaloneFactory) -> Result<BuiltinServerHandler, String> {
    catch_build("builtin handler", || {
        factory.create().map_err(|error| error.to_string())
    })
}

/// 初始化 panic 在 router 边界收敛为能力降级，不终止进程。
fn catch_build<T>(
    capability: &str,
    build: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    match std::panic::catch_unwind(AssertUnwindSafe(build)) {
        Ok(result) => result,
        Err(_) => {
            tracing::error!(
                capability,
                "builtin capability panicked during initialization"
            );
            Err("panicked during initialization".to_owned())
        }
    }
}

fn degrade(capability: &BuiltinCapability, reason: &str) {
    tracing::warn!(
        capability = capability.name,
        reason,
        "builtin capability degraded"
    );
}

/// 单出口 router；能力 handler 与任务归属按会话持有。
pub(crate) struct BuiltinRouter {
    entries: Vec<RouterEntry>,
    disabled: Vec<&'static str>,
    /// 已启用但初始化失败的能力；readiness 必须以 `Degraded` 暴露，不能退化为
    /// “未选择”。
    degraded: Vec<(&'static str, String)>,
    tasks: TaskRoutes,
}

fn disabled_readiness(name: &'static str) -> CapabilityReadiness {
    let declared = find_capability(name).expect("capability names come from the registry");
    CapabilityReadiness {
        capability: name,
        status: CapabilityStatus::Disabled,
        tools: declared
            .tools
            .iter()
            .filter(|tool| tool.model_visible)
            .count(),
        resources: declared.resources.static_resources.len()
            + declared.resources.resource_templates.len(),
    }
}

impl BuiltinRouter {
    /// 测试钩子：注入一个“已启用但初始化失败”的能力状态。
    #[cfg(test)]
    pub(crate) fn mark_degraded(&mut self, capability: &'static str, reason: &str) {
        self.degraded.push((capability, reason.to_owned()));
    }

    /// 测试装配：绕过 in-process 工厂，直接注入能力 handler。
    #[cfg(test)]
    pub(crate) fn for_test(
        handlers: Vec<(&'static str, router_test_support::TestCapability)>,
        disabled: &[&'static str],
    ) -> Result<Self, RouterBuildError> {
        let entries: Vec<RouterEntry> = handlers
            .into_iter()
            .map(|(name, handler)| {
                let declared =
                    find_capability(name).expect("test capabilities come from the registry");
                RouterEntry {
                    declared,
                    handler: CapabilityHandler::Test(handler),
                }
            })
            .collect();
        if !entries.iter().any(RouterEntry::is_ready) {
            return Err(RouterBuildError::NoCapabilities {
                degraded: entries
                    .iter()
                    .map(|entry| format!("{}: degraded", entry.declared.name))
                    .collect(),
            });
        }
        Ok(Self {
            entries,
            disabled: disabled.to_vec(),
            degraded: Vec::new(),
            tasks: TaskRoutes::default(),
        })
    }

    /// 有效能力集（ready），声明顺序。
    pub(crate) fn effective_capabilities(&self) -> Vec<&'static str> {
        self.entries
            .iter()
            .filter(|entry| entry.is_ready())
            .map(|entry| entry.declared.name)
            .collect()
    }

    /// readiness/诊断快照；与 `tools/list`、资源列表使用同一有效能力集。
    /// 宿主面板接入前由 router 测试消费。
    #[allow(dead_code)]
    pub(crate) fn readiness(&self) -> RouterReadiness {
        let mut capabilities: Vec<CapabilityReadiness> =
            self.entries.iter().map(RouterEntry::readiness).collect();
        for name in &self.disabled {
            capabilities.push(disabled_readiness(name));
        }
        for (name, reason) in &self.degraded {
            let mut readiness = disabled_readiness(name);
            readiness.status = CapabilityStatus::Degraded {
                reason: reason.clone(),
            };
            capabilities.push(readiness);
        }
        capabilities.sort_by_key(|readiness| {
            BUILTIN_V5_CAPABILITIES
                .iter()
                .position(|capability| capability.name == readiness.capability)
                .unwrap_or(usize::MAX)
        });
        let effective = self.effective_capabilities();
        RouterReadiness {
            ready: !effective.is_empty(),
            effective,
            capabilities,
        }
    }

    fn ready_entries(&self) -> impl Iterator<Item = &RouterEntry> {
        self.entries.iter().filter(|entry| entry.is_ready())
    }

    fn entry(&self, capability: &str) -> Option<&RouterEntry> {
        self.entries
            .iter()
            .find(|entry| entry.declared.name == capability)
    }

    /// 服务端授权：关闭或降级的能力不可调用。
    fn require_ready(&self, capability: &'static str) -> Result<&RouterEntry, McpError> {
        match self.entry(capability) {
            Some(entry) if entry.is_ready() => Ok(entry),
            Some(entry) => Err(McpError::internal_error(
                format!(
                    "capability `{}` is unavailable: {:?}",
                    capability,
                    entry.status()
                ),
                None,
            )),
            None => match self.degraded.iter().find(|(name, _)| *name == capability) {
                Some((_, reason)) => Err(McpError::internal_error(
                    format!("capability `{capability}` is unavailable: {reason}"),
                    None,
                )),
                None => Err(McpError::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("capability `{capability}` is disabled"),
                    None,
                )),
            },
        }
    }

    fn require_any_ready(&self) -> Result<(), McpError> {
        if self.ready_entries().next().is_some() {
            Ok(())
        } else {
            Err(McpError::internal_error(
                "all builtin capabilities are unavailable",
                None,
            ))
        }
    }

    fn route_resource(&self, uri: &str) -> Result<&RouterEntry, McpError> {
        let capability = capability_of_resource_uri(uri).map_err(|error| {
            McpError::invalid_params(format!("unknown resource URI: {error}"), None)
        })?;
        self.require_ready(capability.name)
    }

    fn route_task(&self, task_id: &str, operation: &str) -> Result<&RouterEntry, McpError> {
        let capability = self.tasks.capability_of(task_id).ok_or_else(|| {
            McpError::new(
                ErrorCode::INVALID_PARAMS,
                format!("unknown task id for {operation}: {task_id}"),
                None,
            )
        })?;
        self.require_ready(capability)
    }

    fn register_task(
        &self,
        capability: &'static str,
        response: &CallToolResponse,
    ) -> Result<(), McpError> {
        if let CallToolResponse::Task(CreateTaskResult { task, .. }) = response {
            self.tasks.register(capability, &task.task_id)?;
        }
        Ok(())
    }
}

/// 能力调用隔离：panic 收敛为该能力的内部错误，其他能力继续服务。
async fn guarded<T>(
    capability: &'static str,
    future: impl std::future::Future<Output = Result<T, McpError>>,
) -> Result<T, McpError> {
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => result,
        Err(_) => {
            tracing::error!(capability, "builtin capability panicked during a request");
            Err(McpError::internal_error(
                format!("capability `{capability}` panicked while handling the request"),
                None,
            ))
        }
    }
}

fn merge_flag(current: Option<bool>, incoming: Option<bool>) -> Option<bool> {
    match (current, incoming) {
        (Some(left), Some(right)) => Some(left || right),
        (left, right) => left.or(right),
    }
}

fn merge_capabilities(
    mut aggregate: ServerCapabilities,
    incoming: &ServerCapabilities,
) -> ServerCapabilities {
    if incoming.tools.is_some() {
        aggregate.tools = Some(ToolsCapability::default());
    }
    if let Some(incoming_resources) = &incoming.resources {
        let slot = aggregate
            .resources
            .get_or_insert_with(ResourcesCapability::default);
        slot.subscribe = merge_flag(slot.subscribe, incoming_resources.subscribe);
        slot.list_changed = merge_flag(slot.list_changed, incoming_resources.list_changed);
    }
    if let Some(incoming_extensions) = &incoming.extensions {
        for key in [TASKS_EXTENSION_ID, SKILLS_EXTENSION_ID] {
            if let Some(settings) = incoming_extensions.get(key) {
                aggregate
                    .extensions
                    .get_or_insert_with(Default::default)
                    .entry(key.to_owned())
                    .or_insert_with(|| settings.clone());
            }
        }
    }
    aggregate
}

impl ServerHandler for BuiltinRouter {
    fn get_info(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::default();
        for entry in self.ready_entries() {
            capabilities = merge_capabilities(capabilities, &entry.handler.get_info().capabilities);
        }
        ServerConfig::new(capabilities).with_server_info(Implementation::new(
            ROUTER_SERVER_NAME,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        for entry in self.ready_entries() {
            entry.handler.on_initialized(context.clone()).await;
        }
    }

    async fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        context: NotificationContext<RoleServer>,
    ) {
        for entry in self.ready_entries() {
            entry
                .handler
                .on_cancelled(notification.clone(), context.clone())
                .await;
        }
    }

    async fn on_progress(
        &self,
        notification: ProgressNotificationParam,
        context: NotificationContext<RoleServer>,
    ) {
        for entry in self.ready_entries() {
            entry
                .handler
                .on_progress(notification.clone(), context.clone())
                .await;
        }
    }

    async fn on_roots_list_changed(&self, context: NotificationContext<RoleServer>) {
        for entry in self.ready_entries() {
            entry.handler.on_roots_list_changed(context.clone()).await;
        }
    }

    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        context: NotificationContext<RoleServer>,
    ) {
        for entry in self.ready_entries() {
            entry
                .handler
                .on_custom_notification(notification.clone(), context.clone())
                .await;
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.require_any_ready()?;
        let mut tools = Vec::new();
        for entry in self.ready_entries() {
            let result = match guarded(
                entry.declared.name,
                entry.handler.list_tools(None, context.clone()),
            )
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    tracing::warn!(
                        capability = entry.declared.name,
                        %error,
                        "skipping capability tools after a failed list"
                    );
                    continue;
                }
            };
            for mut tool in result.tools {
                let Some(declared) = entry
                    .declared
                    .tools
                    .iter()
                    .find(|declared| declared.original_name == tool.name)
                else {
                    tracing::warn!(
                        capability = entry.declared.name,
                        tool = %tool.name,
                        "builtin handler listed a tool missing from the capability registry"
                    );
                    continue;
                };
                if !declared.model_visible {
                    continue;
                }
                // wire 目录保留**原始名**：模型面 `mcp__peri__*` 由 Peri 的工具桥
                // 统一生成（单出口协议层不冒充客户端命名空间）。
                tool.name = declared.original_name.into();
                tools.push(tool);
            }
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.require_any_ready()?;
        // 执行身份是 `(capability, original_name)`；wire 目录与工具桥都使用原始名。
        // 同时接受 v5 模型面名字（`mcp__peri__*`），便于直接使用协议名的客户端；
        // 迁移期旧名仍由 `resolve_model_tool` 显式拒绝。
        let routed = match capability_of_original_tool(&request.name) {
            Some((capability, tool)) => RoutedTool {
                capability: capability.name,
                tool,
            },
            None => resolve_model_tool(&request.name).map_err(|error| {
                McpError::new(ErrorCode::METHOD_NOT_FOUND, error.to_string(), None)
            })?,
        };
        let entry = self.require_ready(routed.capability)?;
        let mut request = request;
        request.name = routed.tool.original_name.into();
        let response = guarded(
            entry.declared.name,
            entry.handler.call_tool(request, context),
        )
        .await?;
        self.register_task(entry.declared.name, &response)?;
        Ok(response)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        self.require_any_ready()?;
        let mut resources: Vec<Resource> = Vec::new();
        for entry in self.ready_entries() {
            let result = match guarded(
                entry.declared.name,
                entry.handler.list_resources(None, context.clone()),
            )
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    tracing::warn!(
                        capability = entry.declared.name,
                        %error,
                        "skipping capability resources after a failed list"
                    );
                    continue;
                }
            };
            for resource in result.resources {
                if entry.accepts_resource(&resource.uri)
                    && !resources
                        .iter()
                        .any(|existing| existing.uri == resource.uri)
                {
                    resources.push(resource);
                }
            }
        }
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        self.require_any_ready()?;
        let mut templates: Vec<rmcp::model::ResourceTemplate> = Vec::new();
        for entry in self.ready_entries() {
            let result = match guarded(
                entry.declared.name,
                entry.handler.list_resource_templates(None, context.clone()),
            )
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    tracing::warn!(
                        capability = entry.declared.name,
                        %error,
                        "skipping capability resource templates after a failed list"
                    );
                    continue;
                }
            };
            for template in result.resource_templates {
                let declared = entry
                    .declared
                    .resources
                    .resource_templates
                    .contains(&template.uri_template.as_str());
                if declared
                    && !templates
                        .iter()
                        .any(|existing| existing.uri_template == template.uri_template)
                {
                    templates.push(template);
                }
            }
        }
        Ok(ListResourceTemplatesResult::with_all_items(templates))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        self.require_any_ready()?;
        let entry = self.route_resource(&request.uri)?;
        guarded(
            entry.declared.name,
            entry.handler.read_resource(request, context),
        )
        .await
    }

    async fn read_resource_directory(
        &self,
        request: ReadResourceDirectoryRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceDirectoryResult, McpError> {
        self.require_any_ready()?;
        let entry = self.route_resource(&request.uri)?;
        guarded(
            entry.declared.name,
            entry.handler.read_resource_directory(request, context),
        )
        .await
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.require_any_ready()?;
        let entry = self.route_resource(&request.uri)?;
        guarded(
            entry.declared.name,
            entry.handler.subscribe(request, context),
        )
        .await
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.require_any_ready()?;
        let entry = self.route_resource(&request.uri)?;
        guarded(
            entry.declared.name,
            entry.handler.unsubscribe(request, context),
        )
        .await
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        let mut uris: Vec<String> = Vec::new();
        let mut accepts_tasks = false;
        for entry in self.ready_entries() {
            accepts_tasks |= entry.supports_tasks();
            for uri in entry.declared.resources.default_subscriptions {
                if !uris.iter().any(|existing| existing == uri) {
                    uris.push((*uri).to_owned());
                }
            }
        }
        if uris.is_empty() && !accepts_tasks {
            return None;
        }
        let mut builder = SubscriptionFilter::builder();
        if !uris.is_empty() {
            builder = builder.resource_subscriptions(uris);
        }
        if accepts_tasks {
            if let Some(task_ids) = &requested.task_ids {
                builder = builder.task_ids(task_ids.clone());
            }
        }
        Some(requested.intersection(&builder.build()))
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        // 现代订阅由单一资源能力承接；多能力 fan-out 需要独立 sink，首阶段不做。
        for entry in self.ready_entries() {
            if entry.accepts_subscriptions() && entry.declared.name == "workspace" {
                return guarded(entry.declared.name, entry.handler.listen(context)).await;
            }
        }
        context.cancelled().await;
        Ok(())
    }

    async fn list_skills(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListSkillsResult, McpError> {
        let entry = self.require_ready("workspace")?;
        guarded(
            entry.declared.name,
            entry.handler.list_skills(request, context),
        )
        .await
    }

    async fn get_skill(
        &self,
        request: GetSkillRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetSkillResult, McpError> {
        let entry = self.require_ready("workspace")?;
        guarded(
            entry.declared.name,
            entry.handler.get_skill(request, context),
        )
        .await
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.is_ready() && entry.declared.name == "workspace")
        {
            return guarded(
                entry.declared.name,
                entry.handler.on_custom_request(request, context),
            )
            .await;
        }
        Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            "custom requests require the workspace capability",
            None,
        ))
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        let entry = self.route_task(&request.task_id, "tasks/get")?;
        guarded(
            entry.declared.name,
            entry.handler.get_task(request, context),
        )
        .await
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let entry = self.route_task(&request.task_id, "tasks/update")?;
        guarded(
            entry.declared.name,
            entry.handler.update_task(request, context),
        )
        .await
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let entry = self.route_task(&request.task_id, "tasks/cancel")?;
        guarded(
            entry.declared.name,
            entry.handler.cancel_task(request, context),
        )
        .await
    }
}

impl std::fmt::Debug for BuiltinRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BuiltinRouter")
            .field("effective", &self.effective_capabilities())
            .field("disabled", &self.disabled)
            .finish()
    }
}

mod readiness;
mod router_handler;
mod tasks;

pub use readiness::{CapabilityReadiness, CapabilityStatus, RouterReadiness};
use tasks::TaskRoutes;

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;

#[cfg(test)]
#[path = "router_test_support.rs"]
pub(crate) mod router_test_support;
