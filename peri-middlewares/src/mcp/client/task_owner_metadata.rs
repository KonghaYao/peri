use std::sync::Arc;

use peri_acp_types::builtin_mcp;
use peri_acp_types::tasks::TaskManager;
use rmcp::model::RequestMetaObject;

use super::McpClientPool;

/// Whether `source` is a session-scoped capability that isolates background
/// tasks by the direct initiating session.
///
/// The scoping fact is the registry field `task_owner_scoped`
/// (`workspace` / `monitor`), not a server-name literal. `WorkspaceRemote`
/// keeps the same contract for the remote workspace endpoint.
pub(crate) fn source_uses_task_owner(source: Option<&crate::mcp::config::ConfigSource>) -> bool {
    match source {
        Some(crate::mcp::config::ConfigSource::WorkspaceRemote) => true,
        Some(crate::mcp::config::ConfigSource::Builtin { instance }) => {
            if builtin_mcp::is_v5_server_name(instance) {
                // 单出口聚合多种能力；服务端级判定用于订阅 / 恢复等 workspace 语义，
                // 工具调用改用 [`tool_uses_task_owner`] 逐能力判定。
                true
            } else {
                builtin_mcp::find(instance).is_some_and(|declared| declared.task_owner_scoped)
            }
        }
        Some(_) | None => false,
    }
}

/// v5 单出口工具所属能力；迁移期与外部 server 返回 `None`。
pub(crate) fn tool_capability(
    source: Option<&crate::mcp::config::ConfigSource>,
    tool_name: &str,
) -> Option<&'static peri_acp_types::builtin_mcp::v5::BuiltinCapability> {
    match source {
        Some(crate::mcp::config::ConfigSource::Builtin { instance })
            if builtin_mcp::is_v5_server_name(instance) =>
        {
            builtin_mcp::capability_of_original_tool(tool_name).map(|(capability, _)| capability)
        }
        _ => None,
    }
}

/// 单次工具调用是否需要注入 `_meta["peri/taskOwner"]`。
///
/// v5 单出口按工具所属能力的 `host_injection.task_owner_scoped` 判定；
/// 迁移期六实例与远端 Workspace 沿用来源级判定。
pub(crate) fn tool_uses_task_owner(
    source: Option<&crate::mcp::config::ConfigSource>,
    tool_name: &str,
) -> bool {
    match source {
        Some(crate::mcp::config::ConfigSource::Builtin { instance })
            if builtin_mcp::is_v5_server_name(instance) =>
        {
            builtin_mcp::capability_of_original_tool(tool_name)
                .is_some_and(|(capability, _)| capability.host_injection.task_owner_scoped)
        }
        _ => source_uses_task_owner(source),
    }
}

/// Build the optional `_meta["peri/taskOwner"]` for a session-scoped request.
///
/// The owner is the session that directly initiated the work. It is a plain
/// identity, not a secret or capability: remote deployments must authenticate
/// the connection itself.
pub(crate) fn task_owner_meta(
    source: Option<&crate::mcp::config::ConfigSource>,
    session_id: &str,
) -> Option<RequestMetaObject> {
    source_uses_task_owner(source).then(|| {
        let mut meta = RequestMetaObject::new();
        meta.0 .0.insert(
            peri_mcp_core::task_owner::TASK_OWNER_META_KEY.into(),
            session_id.into(),
        );
        // ACP 会话身份就是 thread 身份；恢复查询按 thread id 过滤存活任务时需要
        // `peri/threadOwner`。若未来两者分离，这里必须改传真实 thread id。
        meta.0 .0.insert(
            peri_mcp_core::task_owner::THREAD_OWNER_META_KEY.into(),
            session_id.into(),
        );
        meta
    })
}

impl McpClientPool {
    pub fn bind_session_task_manager(&self, session_id: &str, manager: &Arc<dyn TaskManager>) {
        self.session_bindings
            .write()
            .bind_initial_manager(session_id, Arc::clone(manager));
    }

    pub(crate) fn task_owner_meta_for(
        &self,
        server: &str,
        session_id: &str,
    ) -> Option<RequestMetaObject> {
        let resolved = self
            .capability_server_name(server)
            .unwrap_or_else(|| server.to_string());
        let source = self
            .clients
            .read()
            .get(&resolved)
            .and_then(|client| client.source.clone());
        task_owner_meta(source.as_ref(), session_id)
    }
}
