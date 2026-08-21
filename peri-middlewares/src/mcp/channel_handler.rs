use std::sync::Arc;

use peri_agent::interaction::{
    channel_types::{ChannelNotification, PermissionResponse},
    ChannelState,
};
use rmcp::{
    handler::client::ClientHandler,
    model::{CustomNotification, InitializeRequestParams},
    service::{NotificationContext, RoleClient},
};

/// MCP 自定义通知处理器，实现 `ClientHandler` trait
///
/// 作为 MCP client 角色，接收来自 Channel Server 的自定义通知，
/// 根据 `method` 字段路由到 channel 消息推送或权限响应处理。
pub struct ChannelHandler {
    pub state: Arc<ChannelState>,
}

impl ChannelHandler {
    pub fn new(state: Arc<ChannelState>) -> Self {
        Self { state }
    }
}

impl ChannelHandler {
    /// 处理 `notifications/claude/channel` — 频道消息推送
    fn handle_channel_notification(&self, notif: &CustomNotification) {
        let Some(params) = &notif.params else {
            tracing::warn!("channel notification params missing");
            return;
        };

        let Ok(msg) = serde_json::from_value::<ChannelNotification>(params.clone()) else {
            tracing::warn!("channel notification params parse failed");
            return;
        };

        let server_name = extract_server_name(&msg.source);

        let authorized = self.state.authorized.read().contains_key(&server_name);
        if !authorized {
            tracing::warn!(source = %msg.source, "unauthorized channel, ignoring notification");
            return;
        }

        let txs: Vec<_> = self
            .state
            .channel_msg_txs
            .read()
            .values()
            .cloned()
            .collect();
        if txs.is_empty() {
            tracing::warn!("no active sessions to receive channel notification");
            return;
        }

        tracing::info!(source = %msg.source, chat_id = %msg.chat_id, "received channel notification");
        for tx in &txs {
            let _ = tx.send(msg.clone());
        }
    }

    /// 处理 `notifications/claude/permission` — 权限响应
    fn handle_permission_response(&self, notif: &CustomNotification) {
        let Some(params) = &notif.params else {
            tracing::warn!("permission response params missing");
            return;
        };

        let Ok(resp) = serde_json::from_value::<PermissionResponse>(params.clone()) else {
            tracing::warn!("permission response params parse failed");
            return;
        };

        let sender = {
            let mut pending = self.state.pending_permissions.lock();
            pending.remove(&resp.request_id)
        };

        match sender {
            Some(s) => {
                tracing::info!(request_id = %resp.request_id, approved = resp.approved, "channel permission response");
                let _ = s.send(resp);
            }
            None => {
                tracing::warn!(request_id = %resp.request_id, "no pending permission request found");
            }
        }
    }
}

impl ClientHandler for ChannelHandler {
    fn get_info(&self) -> InitializeRequestParams {
        super::client::mcpp_client_info()
    }

    // rmcp trait 要求返回 impl Future，无法改为 async fn
    #[allow(clippy::manual_async_fn)]
    fn on_custom_notification(
        &self,
        notification: CustomNotification,
        context: NotificationContext<RoleClient>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        async move {
            match notification.method.as_str() {
                "notifications/claude/channel" => {
                    self.handle_channel_notification(&notification);
                }
                "notifications/claude/permission" => {
                    self.handle_permission_response(&notification);
                }
                _ => {
                    let _ = (notification, context);
                    tracing::debug!("unhandled custom notification");
                }
            }
        }
    }
}

/// 从 channel source 标识符提取 MCP server name（对齐 config 中的命名格式）
///
/// plugin 格式移除 @marketplace 保留 `plugin:{name}:{server}`：
/// - `"plugin:weixin@anthropic:weixin"` → `"plugin:weixin:weixin"`
/// - `"plugin:weixin:weixin"` → `"plugin:weixin:weixin"`
///
/// server 格式直接取出 server name：
/// - `"server:my-mcp"` → `"my-mcp"`
fn extract_server_name(source: &str) -> String {
    if let Some(rest) = source.strip_prefix("plugin:") {
        // 移除 @marketplace 部分：从 "@anthropic:server" 中删掉 "@anthropic"
        let cleaned = if let Some(at_pos) = rest.find('@') {
            if let Some(colon_pos) = rest[at_pos..].find(':') {
                format!("{}{}", &rest[..at_pos], &rest[at_pos + colon_pos..])
            } else {
                rest[..at_pos].to_string()
            }
        } else {
            rest.to_string()
        };
        format!("plugin:{}", cleaned)
    } else if let Some(rest) = source.strip_prefix("server:") {
        rest.to_string()
    } else {
        source.to_string()
    }
}
