use super::*;
use peri_acp_types::plugin::McpSubscriptionsConfig;

// 3.0 批 2 波 2：装配注入端口实现（ACP 侧只持 `Arc<dyn McpPoolPort>`）。
// M-TUI 收口：`shutdown`（host/shutdown 命令面）与 `snapshot`（mcp/list
// 命令面）为新增数据端口；TUI 不再直持池句柄与 watch channel。
#[async_trait::async_trait]
impl peri_acp_types::ports::McpPoolPort for McpClientPool {
    async fn control_mcp(
        self: Arc<Self>,
        request: peri_acp_types::mcp_control::McpControlRequest,
    ) -> Result<
        peri_acp_types::mcp_control::McpControlResponse,
        peri_acp_types::mcp_control::McpControlError,
    > {
        McpClientPool::control_mcp(self, request).await
    }
    fn catalog_source(
        self: Arc<Self>,
        session_id: &str,
    ) -> Option<Arc<dyn peri_acp_types::mcp_control::SessionMcpCatalogPort>> {
        Some(crate::mcp::control::catalog_source(self, session_id))
    }
    fn cancel_oauth_flow(&self, flow_id: &str) -> Result<bool, String> {
        Ok(McpClientPool::cancel_oauth_flow(self, flow_id))
    }
    fn verify_shared_environment_close(&self, root_session_id: &str) -> Result<(), String> {
        self.session_bindings
            .read()
            .verify_environment_close(root_session_id)
    }

    fn release_agent_session(&self, session_id: &str) -> Result<(), String> {
        self.session_bindings.write().release(session_id);
        Ok(())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn rewind_files(
        &self,
        session_id: &str,
        changes: serde_json::Value,
    ) -> Result<(), String> {
        self.rewind_workspace_files(session_id, changes).await
    }

    fn has_active_tasks(&self, session_id: &str) -> bool {
        self.session_bindings
            .read()
            .manager(session_id)
            .is_some_and(|manager| manager.has_unsettled_external())
    }

    fn has_instance(&self, instance: &str) -> bool {
        McpClientPool::get_client(self, instance).is_some()
    }

    fn bind_agent_session(
        &self,
        session_id: &str,
        inbox: InboxHandle,
        manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    ) {
        self.bind_session_task_manager(session_id, &manager);
        self.register_inbox(session_id, inbox);
    }

    fn agent_session_binding(
        &self,
        session_id: &str,
    ) -> Option<(InboxHandle, Arc<dyn peri_acp_types::tasks::TaskManager>)> {
        let (inbox, manager) = self.session_bindings.read().binding(session_id)?;
        if manager
            .as_any()
            .downcast_ref::<peri_agent::agent::async_tasks::TaskManager>()
            .is_some_and(|manager| manager.session_close_settled())
        {
            return None;
        }
        Some((inbox, manager))
    }

    fn begin_shutdown(&self) {
        McpClientPool::begin_shutdown(self);
    }

    async fn shutdown(&self) -> McpPoolShutdownReport {
        McpClientPool::shutdown(self).await
    }

    fn snapshot(&self) -> serde_json::Value {
        let init_phase = match &*self.init_status.read() {
            McpInitStatus::Pending => "pending",
            McpInitStatus::Initializing { .. } => "initializing",
            McpInitStatus::Ready { .. } => "ready",
            McpInitStatus::Failed(_) => "failed",
        };
        let infos = self.all_server_infos();
        serde_json::json!({
            "initPhase": init_phase,
            "servers": infos.iter().map(|info| serde_json::json!({
                "name": info.name.clone(),
                "status": format!("{:?}", info.status).to_lowercase(),
                "transport": info.transport_type.clone(),
                "toolsCount": info.tool_count,
            })).collect::<Vec<_>>(),
        })
    }

    // ── W3 端口补全：委托既有固有方法，语义逐项对齐（见 ports.rs 契约文档）──

    fn server_infos(&self) -> Result<Vec<McpServerInfo>, String> {
        Ok(McpClientPool::all_server_infos(self)
            .into_iter()
            .map(|info| {
                let control = self.control_descriptor(&info.name);
                let mut projected = mcp_server_info_projection(info);
                projected.control = control;
                projected
            })
            .collect())
    }

    fn active_oauth_flow(&self, server_name: &str) -> Option<String> {
        McpClientPool::active_oauth_flow(self, server_name)
    }

    fn spawn_oauth_flow_with_id(
        self: Arc<Self>,
        server_name: &str,
        flow_id: &str,
    ) -> Result<McpOAuthStartDisposition, String> {
        Ok(
            match McpClientPool::spawn_oauth_flow_with_id(&self, server_name, flow_id) {
                OAuthStartDisposition::Started => McpOAuthStartDisposition::Started,
                OAuthStartDisposition::AlreadyActive => McpOAuthStartDisposition::AlreadyActive,
                OAuthStartDisposition::Conflict { active_flow_id } => {
                    McpOAuthStartDisposition::Conflict { active_flow_id }
                }
            },
        )
    }

    fn deliver_oauth_callback(
        &self,
        server_name: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        McpClientPool::deliver_oauth_callback(self, server_name, code, state)
    }

    fn cancel_oauth_callback(&self, server_name: &str) -> Result<bool, String> {
        Ok(McpClientPool::cancel_oauth_callback(self, server_name))
    }

    async fn close_agent_session_scope(self: Arc<Self>, session_id: &str) -> Result<(), String> {
        self.cancel_session_external_tasks(session_id).await
    }

    async fn cancel_session_external_tasks(&self, session_id: &str) -> Result<(), String> {
        McpClientPool::cancel_session_external_tasks(self, session_id).await
    }

    fn bind_session_task_manager(&self, session_id: &str, manager: &Arc<dyn TaskManager>) {
        McpClientPool::bind_session_task_manager(self, session_id, manager);
    }

    fn attach_connection_notifier(
        self: Arc<Self>,
        registry: Option<&Arc<McpSkillRegistry>>,
        command_registry: Option<&Arc<CommandRegistry>>,
        cancel: &tokio_util::sync::CancellationToken,
    ) {
        super::super::middleware::attach_connection_notifier(
            &self,
            registry,
            command_registry,
            cancel,
            None,
        );
    }

    fn prewarm_discovery(
        self: Arc<Self>,
        registry: &Arc<McpSkillRegistry>,
        command_registry: &Arc<CommandRegistry>,
        session_id: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) {
        super::super::middleware::prewarm_discovery(
            &self,
            registry,
            command_registry,
            session_id,
            cancel,
        );
    }

    // ── 按实例消费（宿主面） ────────────────────────────────────────────

    async fn call_instance_tool(
        &self,
        instance: &str,
        tool: &str,
        arguments: serde_json::Value,
        request_meta: Option<serde_json::Value>,
    ) -> Result<String, String> {
        McpClientPool::call_instance_tool(self, instance, tool, arguments, request_meta).await
    }

    async fn read_instance_resource(&self, instance: &str, uri: &str) -> Result<String, String> {
        McpClientPool::read_instance_resource(self, instance, uri).await
    }

    async fn subscribe_instance_resource(
        &self,
        instance: &str,
        uri: &str,
    ) -> Result<peri_acp_types::ports::McpResourceSubscription, String> {
        McpClientPool::subscribe_instance_resource(self, instance, uri).await
    }

    fn workspace_source(&self) -> Option<peri_acp_types::plugin::ConfigSource> {
        McpClientPool::workspace_source(self)
    }

    fn builtin_workspace_state(&self) -> McpBuiltinWorkspaceState {
        match McpClientPool::get_client(self, "workspace") {
            None => McpBuiltinWorkspaceState::Absent,
            Some(handle) if matches!(handle.status, ClientStatus::Connected) => {
                McpBuiltinWorkspaceState::Connected
            }
            Some(_) => McpBuiltinWorkspaceState::NotConnected,
        }
    }

    async fn read_skills_catalog(&self, session_id: &str) -> Result<Vec<SkillMetadata>, String> {
        McpClientPool::read_skills_catalog(self, session_id).await
    }

    async fn read_builtin_workspace_instructions(
        &self,
    ) -> Result<(Option<String>, Option<String>), String> {
        McpClientPool::read_builtin_workspace_instructions(self).await
    }

    async fn read_builtin_workspace_meta(
        &self,
        enabled_sections: &HashSet<String>,
    ) -> Result<HashMap<String, String>, String> {
        McpClientPool::read_builtin_workspace_meta(self, enabled_sections).await
    }
}

/// `mcp/list` 单项列表上限：工具、资源、订阅资源各自最多透传 64 条。
const MCP_LIST_MAX_ENTRIES: usize = 64;
/// `mcp/list` 文本字段上限（清洗后的字符数）。
const MCP_LIST_MAX_TEXT: usize = 160;

/// 不可信 MCP 文本清洗：控制字符替换为空格、trim、截断到 160 字符。
fn sanitize_mcp_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .chars()
        .take(MCP_LIST_MAX_TEXT)
        .collect()
}

/// 可选文本清洗：空串归一为 None（描述/名称缺省语义）。
fn sanitize_mcp_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| sanitize_mcp_text(&text))
        .filter(|text| !text.is_empty())
}

/// `mcp/list` 契约投影：暴露面板需要的状态分类与排障字段（有界的
/// `error_summary`、`url`；摘要已由 `ServerInfo` 侧 trim + 160 字符截断，
/// 完整错误链不出投影），其余内部字段（source/plugin_source/cache_version
/// 等）不透传。
///
/// 工具/资源/订阅资源同时做条目与文本双重有界：不可信 MCP server 返回的
/// 名称与描述可能含控制字符或超长正文，进入面板前统一清洗；全量计数仍由
/// `tool_count`/`resource_count` 承载。
fn mcp_server_info_projection(info: ServerInfo) -> McpServerInfo {
    let tool_count = info.tool_count;
    let resource_count = info.resource_count;
    McpServerInfo {
        name: info.name,
        transport: info.transport_type,
        status: match info.status {
            ClientStatus::Connected => McpServerConnectionStatus::Connected,
            ClientStatus::Failed(_) => McpServerConnectionStatus::Failed,
            ClientStatus::Disconnected => McpServerConnectionStatus::Disconnected,
            ClientStatus::Disabled => McpServerConnectionStatus::Disabled,
            ClientStatus::Uninitialized => McpServerConnectionStatus::Uninitialized,
        },
        oauth_status: match info.oauth_status {
            OAuthStatus::None => McpServerOAuthStatus::None,
            OAuthStatus::Authorized => McpServerOAuthStatus::Authorized,
            OAuthStatus::NeedsAuthorization => McpServerOAuthStatus::NeedsAuthorization,
        },
        tool_count,
        resource_count,
        tools: info
            .tools
            .into_iter()
            .take(MCP_LIST_MAX_ENTRIES)
            .map(|tool| McpToolInfo {
                name: sanitize_mcp_text(&tool.name),
                description: sanitize_mcp_optional(tool.description),
            })
            .collect(),
        resources: info
            .resources
            .into_iter()
            .take(MCP_LIST_MAX_ENTRIES)
            .map(|resource| McpResourceInfo {
                uri: sanitize_mcp_text(&resource.uri),
                name: sanitize_mcp_optional(resource.name),
                description: sanitize_mcp_optional(resource.description),
            })
            .collect(),
        subscriptions: info.subscriptions.map(|subscriptions| {
            let McpSubscriptionsConfig {
                resources,
                tools_list_changed,
                prompts_list_changed,
                resources_list_changed,
            } = subscriptions;
            McpSubscriptionsConfig {
                resources: resources
                    .into_iter()
                    .take(MCP_LIST_MAX_ENTRIES)
                    .map(|uri| sanitize_mcp_text(&uri))
                    .collect(),
                tools_list_changed,
                prompts_list_changed,
                resources_list_changed,
            }
        }),
        version: info.version,
        connected_at: info.connected_at,
        protocol_version: info.protocol_version,
        error_summary: info.error_summary,
        url: info.url,
        control: Default::default(),
    }
}

/// `McpSubscriptionPort` 实现：SessionManager（peri-acp）在 session 创建 /
/// 销毁时注册 / 注销 inbox；订阅通知到达时经 inbox 唤醒 agent。
impl McpSubscriptionPort for McpClientPool {
    fn register_inbox(&self, session_id: &str, handle: InboxHandle) {
        self.session_bindings
            .write()
            .register_initial_inbox(session_id, handle);
    }

    fn unregister_inbox(&self, session_id: &str) {
        self.session_bindings.write().unregister(session_id);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
