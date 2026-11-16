//! Current connection queries and resource projections.
use super::{Arc, ClientStatus, McpClientHandle, McpClientPool, Resource, Tool};
impl McpClientPool {
    #[must_use]
    pub fn tools_for_server(&self, name: &str) -> Vec<Tool> {
        self.clients
            .read()
            .get(name)
            .map(|h| h.tools.clone())
            .unwrap_or_default()
    }
    #[must_use]
    pub fn resources_for_server(&self, name: &str) -> Vec<Resource> {
        self.clients
            .read()
            .get(name)
            .map(|h| h.resources.clone())
            .unwrap_or_default()
    }
    pub fn client(&self, name: &str) -> Option<Arc<McpClientHandle>> {
        self.clients.read().get(name).cloned()
    }

    /// 池内是否配置了 v5 单出口 `peri`（会话声明优先；未连接也算配置事实）。
    pub(crate) fn v5_single_exit_configured(&self) -> bool {
        // 已连接的 `peri` 是比配置快照更早可用的事实：print/bare 路径可能在
        // 会话声明写入前就登记后台任务，配置快照尚未包含注入条目。
        if self
            .clients
            .read()
            .contains_key(peri_acp_types::builtin_mcp::BUILTIN_V5_SERVER_NAME)
        {
            return true;
        }
        let is_peri = |config: &peri_acp_types::plugin::McpServerConfig| {
            matches!(
                &config.source,
                Some(peri_acp_types::plugin::ConfigSource::Builtin { instance })
                    if peri_acp_types::builtin_mcp::is_v5_server_name(instance)
            )
        };
        if let Some(servers) = self.session_servers.get() {
            if let Some(config) = servers.get(peri_acp_types::builtin_mcp::BUILTIN_V5_SERVER_NAME) {
                return is_peri(config);
            }
        }
        self.configs
            .read()
            .get(peri_acp_types::builtin_mcp::BUILTIN_V5_SERVER_NAME)
            .is_some_and(is_peri)
    }

    /// 能力实际落点的 server 名：v5 单出口为 `peri`，迁移期为同名实例。
    ///
    /// `None` = 该能力没有已配置的 server（能力不可用），不是「未连接」。
    pub(crate) fn capability_server_name(&self, capability: &str) -> Option<String> {
        peri_acp_types::builtin_mcp::capability(capability)?;
        // 远端 Workspace 是宿主任命的外部执行环境；v5 迁移期仍以 `workspace`
        // 独立条目存在，不并入 `peri`（代理接线见 active spec 的未验证项）。
        if capability == "workspace" {
            let source = self
                .session_servers
                .get()
                .and_then(|servers| servers.get("workspace"))
                .and_then(|config| config.source.clone())
                .or_else(|| {
                    self.configs
                        .read()
                        .get("workspace")
                        .and_then(|config| config.source.clone())
                });
            if matches!(
                source,
                Some(peri_acp_types::plugin::ConfigSource::WorkspaceRemote)
            ) {
                return Some("workspace".to_string());
            }
        }
        if self.v5_single_exit_configured() {
            return Some(peri_acp_types::builtin_mcp::BUILTIN_V5_SERVER_NAME.to_string());
        }
        let configured = self
            .session_servers
            .get()
            .is_some_and(|servers| servers.contains_key(capability))
            || self.configs.read().contains_key(capability);
        if configured || self.client(capability).is_some() {
            Some(capability.to_string())
        } else {
            None
        }
    }

    /// 能力落点的连接句柄（v5 单出口 / 迁移期同名实例）。
    pub(crate) fn capability_client(&self, capability: &str) -> Option<Arc<McpClientHandle>> {
        self.capability_server_name(capability)
            .and_then(|server| self.client(&server))
    }

    /// 能力落点的会话可见句柄。
    pub(crate) fn capability_client_visible_to(
        &self,
        capability: &str,
        session_id: Option<&str>,
    ) -> Option<Arc<McpClientHandle>> {
        let server = self.capability_server_name(capability)?;
        self.client_visible_to(&server, session_id)
    }

    /// 能力落点的来源身份（未连接也可读；用于执行环境判定）。
    pub(crate) fn capability_source(
        &self,
        capability: &str,
    ) -> Option<peri_acp_types::plugin::ConfigSource> {
        let server = self.capability_server_name(capability)?;
        if let Some(servers) = self.session_servers.get() {
            if let Some(config) = servers.get(&server) {
                return config.source.clone();
            }
        }
        self.configs
            .read()
            .get(&server)
            .and_then(|config| config.source.clone())
            .or_else(|| {
                self.client(&server)
                    .and_then(|handle| handle.source.clone())
            })
    }

    /// 会话可见的连接句柄（[`Self::client`] 的 ACP 归属过滤版）。
    pub fn client_visible_to(
        &self,
        name: &str,
        session_id: Option<&str>,
    ) -> Option<Arc<McpClientHandle>> {
        if session_id.is_some_and(|session_id| !self.is_visible_to_session(name, session_id)) {
            return None;
        }
        self.client(name)
    }
    #[must_use]
    pub fn clients(&self) -> Vec<Arc<McpClientHandle>> {
        self.clients
            .read()
            .values()
            .filter(|c| matches!(c.status, ClientStatus::Connected) && c.call_gate.is_open())
            .cloned()
            .collect()
    }
    /// 会话可见的连接句柄（[`Self::clients`] 的 ACP 归属过滤版）。
    ///
    /// `session_id` 为 `None` 表示不过滤（部署面视图：面板、命令面、池关闭）。
    #[must_use]
    pub fn clients_visible_to(&self, session_id: Option<&str>) -> Vec<Arc<McpClientHandle>> {
        let mut clients = self.clients();
        if let Some(session_id) = session_id {
            clients.retain(|client| self.is_visible_to_session(&client.name, session_id));
        }
        clients
    }
    #[must_use]
    pub fn has_resources(&self) -> bool {
        self.clients
            .read()
            .values()
            .any(|c| matches!(c.status, ClientStatus::Connected) && !c.resources.is_empty())
    }
    #[must_use]
    pub fn resource_summary(&self) -> String {
        let clients = self.clients.read();
        let mut resource_clients: Vec<_> = clients
            .values()
            .filter(|c| matches!(c.status, ClientStatus::Connected) && !c.resources.is_empty())
            .collect();
        // Server membership has no ordering; prompt text must not inherit HashMap iteration.
        resource_clients.sort_by(|left, right| left.name.cmp(&right.name));
        resource_clients
            .into_iter()
            .map(|c| {
                format!(
                    "- server \"{}\": {} ({} resources)",
                    c.name,
                    c.resources
                        .iter()
                        .map(|r| r.uri.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                    c.resources.len()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
