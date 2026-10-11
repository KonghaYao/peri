use super::*;

// 3.0 批 2 波 2：装配注入端口实现（ACP 侧只持 `Arc<dyn McpPoolPort>`）。
// M-TUI 收口：`shutdown`（host/shutdown 命令面）与 `snapshot`（mcp/list
// 命令面）为新增数据端口；TUI 不再直持池句柄与 watch channel。
#[async_trait::async_trait]
impl peri_acp_types::ports::McpPoolPort for McpClientPool {
    fn verify_shared_environment_close(&self, root_session_id: &str) -> Result<(), String> {
        self.session_bindings
            .read()
            .verify_environment_close(root_session_id)
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
            .map(mcp_server_info_projection)
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

    fn deliver_dynamic_oauth_callback(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        McpClientPool::deliver_dynamic_oauth_callback(self, instance, flow_id, code, state)
    }

    fn cancel_oauth_callback(&self, server_name: &str) -> Result<bool, String> {
        Ok(McpClientPool::cancel_oauth_callback(self, server_name))
    }

    fn cancel_dynamic_oauth_flow(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
    ) -> Result<bool, String> {
        Ok(McpClientPool::cancel_dynamic_oauth_flow(
            self, instance, flow_id,
        ))
    }

    async fn open_workspace_task_scope(&self, session_id: &str) -> Result<(), String> {
        McpClientPool::open_workspace_task_scope(self, session_id).await
    }

    async fn close_workspace_task_scope(self: Arc<Self>, session_id: &str) -> Result<(), String> {
        McpClientPool::close_workspace_task_scope(&self, session_id).await
    }

    async fn close_agent_session_scope(self: Arc<Self>, session_id: &str) -> Result<(), String> {
        McpClientPool::close_workspace_task_scope(&self, session_id).await
    }

    async fn reconcile_closing_workspace_scope(&self, session_id: &str) -> Result<(), String> {
        McpClientPool::reconcile_closing_workspace_scope(self, session_id).await
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

    async fn read_builtin_workspace_skills(&self) -> Result<Vec<SkillMetadata>, String> {
        McpClientPool::read_builtin_workspace_skills(self).await
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

/// `mcp/list` 契约投影：暴露面板需要的状态分类与排障字段（有界的
/// `error_summary`、`url`；摘要已由 `ServerInfo` 侧 trim + 160 字符截断，
/// 完整错误链不出投影），其余内部字段（source/plugin_source/cache_version
/// 等）不透传。
fn mcp_server_info_projection(info: ServerInfo) -> McpServerInfo {
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
        tool_count: info.tool_count,
        resource_count: info.resource_count,
        version: info.version,
        connected_at: info.connected_at,
        protocol_version: info.protocol_version,
        error_summary: info.error_summary,
        url: info.url,
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
        self.task_scope_tokens.write().remove(session_id);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
