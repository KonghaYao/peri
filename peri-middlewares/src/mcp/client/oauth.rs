//! OAuth flow 的 scoped identity、准入和 callback 投递；流程执行仍在 client_oauth。

use super::{McpClientPool, McpConnectionKey};
use crate::mcp::oauth_flow::{OAuthCallbackResult, OAuthFlowEvent};
use peri_acp_types::dynamic_mcp::DynamicMcpInstanceKey;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct OAuthFlowKey {
    connection: McpConnectionKey,
    flow_id: String,
}

pub(super) struct PendingOAuthCallback {
    flow_id: String,
    tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthStartDisposition {
    Started,
    AlreadyActive,
    Conflict { active_flow_id: String },
}

impl McpClientPool {
    /// 注入 OAuth 流程事件回调（host 装配面在 `run_initialize` 前调用；
    /// 回调负责把 `AuthorizationNeeded` 的 `callback_tx` 注册进
    /// `pending_oauth_callbacks`，并将事件转发为 ACP 通知）。
    pub fn set_oauth_event_callback<F>(&self, callback: F)
    where
        F: Fn(OAuthFlowEvent) + Send + Sync + 'static,
    {
        let _admission = self.lifecycle_registration.lock();
        if self.is_open() {
            *self.oauth_event_callback.write() = Some(Arc::new(callback));
        }
    }

    /// 读取 OAuth 流程事件回调（spawn 授权任务时克隆给 `OAuthFlowManager`）。
    pub(crate) fn oauth_event_callback(&self) -> Option<Arc<dyn Fn(OAuthFlowEvent) + Send + Sync>> {
        self.oauth_event_callback.read().clone()
    }

    /// 注册待完成 OAuth 授权的回调通道（`AuthorizationNeeded` 事件处理时调用）。
    pub fn register_oauth_callback(
        &self,
        server_name: &str,
        flow_id: &str,
        callback_tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
    ) -> bool {
        self.register_oauth_callback_scoped(
            McpConnectionKey::static_server(server_name),
            flow_id,
            callback_tx,
        )
    }

    pub fn register_dynamic_oauth_callback(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
        callback_tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
    ) -> bool {
        self.register_oauth_callback_scoped(
            McpConnectionKey::dynamic(instance),
            flow_id,
            callback_tx,
        )
    }

    pub(crate) fn register_oauth_callback_scoped(
        &self,
        connection: McpConnectionKey,
        flow_id: &str,
        callback_tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
    ) -> bool {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return false;
        }
        let mut active = self.active_oauth_flows.lock();
        match active.get(&connection) {
            Some(current) if current != flow_id => return false,
            Some(_) => {}
            None => {
                active.insert(connection.clone(), flow_id.to_string());
            }
        }
        drop(active);
        self.pending_oauth_callbacks.lock().insert(
            OAuthFlowKey {
                connection,
                flow_id: flow_id.to_string(),
            },
            PendingOAuthCallback {
                flow_id: flow_id.to_string(),
                tx: callback_tx,
            },
        );
        true
    }

    /// 投递授权码回传（`mcp/oauth_callback` RPC 调用）：查表取 `callback_tx`
    /// 投递 `{code, state}`；无 pending 通道返回错误。
    pub fn deliver_oauth_callback(
        &self,
        server_name: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        let connection = McpConnectionKey::static_server(server_name);
        let flow_id = self
            .active_oauth_flow_scoped(&connection)
            .ok_or_else(|| format!("{server_name} 无进行中的 OAuth 授权"))?;
        let pending = self
            .pending_oauth_callbacks
            .lock()
            .remove(&OAuthFlowKey {
                connection,
                flow_id,
            })
            .ok_or_else(|| format!("{server_name} 无进行中的 OAuth 授权"))?;
        pending
            .tx
            .send(OAuthCallbackResult { code, state })
            .map_err(|_| format!("{server_name} OAuth 授权流程已结束"))
    }

    pub fn deliver_dynamic_oauth_callback(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        self.deliver_oauth_callback_scoped(
            &McpConnectionKey::dynamic(instance),
            flow_id,
            code,
            state,
        )
    }

    /// 以完整 dynamic identity 精确投递授权码。调用方必须持有当前
    /// `(session_id, incarnation_id, flow_id)`，禁止退化为裸 server name。
    pub(super) fn deliver_oauth_callback_scoped(
        &self,
        connection: &McpConnectionKey,
        flow_id: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        if !connection.is_dynamic() {
            return Err("scoped OAuth callback requires dynamic identity".to_string());
        }
        let key = OAuthFlowKey {
            connection: connection.clone(),
            flow_id: flow_id.to_string(),
        };
        let pending = self
            .pending_oauth_callbacks
            .lock()
            .remove(&key)
            .filter(|pending| pending.flow_id == flow_id)
            .ok_or_else(|| "OAuth flow 不再等待 callback".to_string())?;
        pending
            .tx
            .send(OAuthCallbackResult { code, state })
            .map_err(|_| "OAuth flow 已结束".to_string())
    }

    /// 以 flow identity 精确投递授权码。Hub 不使用该接口；保留给未来安全
    /// 手动 callback 能力，避免 server-name-only 的错误归属。
    pub fn deliver_oauth_callback_for_flow(
        &self,
        flow_id: &str,
        code: String,
        state: String,
    ) -> Result<(), String> {
        let connection = self
            .connection_for_oauth_flow(flow_id)
            .ok_or_else(|| "OAuth flow 不存在".to_string())?;
        let key = OAuthFlowKey {
            connection,
            flow_id: flow_id.to_string(),
        };
        let pending = self
            .pending_oauth_callbacks
            .lock()
            .remove(&key)
            .filter(|pending| pending.flow_id == flow_id)
            .ok_or_else(|| "OAuth flow 不再等待 callback".to_string())?;
        pending
            .tx
            .send(OAuthCallbackResult { code, state })
            .map_err(|_| "OAuth flow 已结束".to_string())
    }

    /// 取消进行中的 OAuth 授权（`mcp/oauth_cancel` RPC 调用）：移除 pending
    /// 通道并 drop sender，后台 `run_oauth_flow` 收到 Cancelled 终止。
    pub fn cancel_oauth_callback(&self, server_name: &str) -> bool {
        let connection = McpConnectionKey::static_server(server_name);
        let flow_id = self.active_oauth_flows.lock().get(&connection).cloned();
        flow_id
            .as_deref()
            .map(|flow_id| self.cancel_oauth_flow(flow_id))
            .unwrap_or(false)
    }

    pub fn cancel_dynamic_oauth_flow(
        &self,
        instance: DynamicMcpInstanceKey,
        flow_id: &str,
    ) -> bool {
        self.cancel_oauth_flow_scoped(&McpConnectionKey::dynamic(instance), flow_id)
    }

    pub(super) fn cancel_oauth_flow_scoped(
        &self,
        connection: &McpConnectionKey,
        flow_id: &str,
    ) -> bool {
        if !connection.is_dynamic() {
            return false;
        }
        self.pending_oauth_callbacks
            .lock()
            .remove(&OAuthFlowKey {
                connection: connection.clone(),
                flow_id: flow_id.to_string(),
            })
            .is_some_and(|pending| pending.flow_id == flow_id)
    }

    /// 精确取消一个活跃 flow；不接受由客户端指定的 server 名称。
    pub fn cancel_oauth_flow(&self, flow_id: &str) -> bool {
        let Some(connection) = self.connection_for_oauth_flow(flow_id) else {
            return false;
        };
        self.pending_oauth_callbacks
            .lock()
            .remove(&OAuthFlowKey {
                connection,
                flow_id: flow_id.to_string(),
            })
            .is_some_and(|pending| pending.flow_id == flow_id)
    }

    pub fn active_oauth_flow(&self, server_name: &str) -> Option<String> {
        self.active_oauth_flow_scoped(&McpConnectionKey::static_server(server_name))
    }

    pub(crate) fn active_oauth_flow_scoped(&self, connection: &McpConnectionKey) -> Option<String> {
        self.active_oauth_flows.lock().get(connection).cloned()
    }

    pub(crate) fn reserve_oauth_flow(
        &self,
        server_name: &str,
        flow_id: &str,
    ) -> OAuthStartDisposition {
        self.reserve_oauth_flow_scoped(McpConnectionKey::static_server(server_name), flow_id)
    }

    pub(crate) fn reserve_oauth_flow_scoped(
        &self,
        connection: McpConnectionKey,
        flow_id: &str,
    ) -> OAuthStartDisposition {
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return OAuthStartDisposition::Conflict {
                active_flow_id: "pool-closing".to_string(),
            };
        }
        let mut active = self.active_oauth_flows.lock();
        match active.get(&connection) {
            Some(current) if current == flow_id => OAuthStartDisposition::AlreadyActive,
            Some(current) => OAuthStartDisposition::Conflict {
                active_flow_id: current.clone(),
            },
            None => {
                active.insert(connection, flow_id.to_string());
                OAuthStartDisposition::Started
            }
        }
    }

    pub fn release_oauth_flow(&self, server_name: &str, flow_id: &str) {
        self.release_oauth_flow_scoped(&McpConnectionKey::static_server(server_name), flow_id);
    }

    pub(crate) fn release_oauth_flow_scoped(&self, connection: &McpConnectionKey, flow_id: &str) {
        let mut active = self.active_oauth_flows.lock();
        if active
            .get(connection)
            .is_some_and(|current| current == flow_id)
        {
            active.remove(connection);
        }
        drop(active);
        self.pending_oauth_callbacks.lock().remove(&OAuthFlowKey {
            connection: connection.clone(),
            flow_id: flow_id.to_string(),
        });
    }

    pub(crate) fn revoke_oauth_connection(&self, connection: &McpConnectionKey) {
        let flow_id = self.active_oauth_flows.lock().remove(connection);
        if let Some(flow_id) = flow_id {
            self.pending_oauth_callbacks.lock().remove(&OAuthFlowKey {
                connection: connection.clone(),
                flow_id,
            });
        }
    }

    fn connection_for_oauth_flow(&self, flow_id: &str) -> Option<McpConnectionKey> {
        self.active_oauth_flows
            .lock()
            .iter()
            .find_map(|(connection, active)| (active == flow_id).then(|| connection.clone()))
    }
}
