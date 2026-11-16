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
