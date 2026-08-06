use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::sync::oneshot;

use peri_acp_types::interaction::{
    short_request_id, ApprovalDecision, ApprovalItem, ChannelNotificationSender, ChannelState,
    InteractionContext, InteractionResponse, PermissionRequest, UserInteractionBroker,
};

/// 对 MCP Channel 发起权限审批的 broker
///
/// 使用 ChannelNotificationSender 发送 permission_request 到所有已授权 channel，
/// 在 pending_permissions 中注册 oneshot 等待响应，5 分钟超时。
pub struct ChannelBroker {
    pub state: Arc<ChannelState>,
    pub sender: Arc<dyn ChannelNotificationSender>,
}

impl ChannelBroker {
    pub fn new(state: Arc<ChannelState>, sender: Arc<dyn ChannelNotificationSender>) -> Self {
        Self { state, sender }
    }
}

#[async_trait]
impl UserInteractionBroker for ChannelBroker {
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse {
        match ctx {
            InteractionContext::Approval { items } => self.request_approval(items).await,
            InteractionContext::Questions { .. } => {
                // Channel doesn't support interactive questions
                InteractionResponse::Answers(vec![])
            }
        }
    }
}

impl ChannelBroker {
    async fn request_approval(&self, items: Vec<ApprovalItem>) -> InteractionResponse {
        let authorized_servers: Vec<(String, String)> = {
            self.state
                .authorized
                .read()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };

        if authorized_servers.is_empty() {
            return InteractionResponse::Decisions(
                items
                    .iter()
                    .map(|_| ApprovalDecision::Reject {
                        reason: "no authorized channels".to_string(),
                        source: None,
                    })
                    .collect(),
            );
        }

        let request_id = short_request_id();
        let (tx, rx) = oneshot::channel();

        // Register pending permission
        {
            let mut pending = self.state.pending_permissions.lock();
            pending.insert(request_id.clone(), tx);
        }

        // Send permission_request to all authorized channels
        for (server_name, _source) in &authorized_servers {
            for item in &items {
                let req = PermissionRequest {
                    request_id: request_id.clone(),
                    tool_name: item.tool_name.clone(),
                    arguments: item.tool_input.clone(),
                    source: "peri".to_string(),
                };

                if let Err(e) = self
                    .sender
                    .send_notification(
                        server_name,
                        "notifications/claude/permission_request",
                        serde_json::to_value(&req).unwrap_or_default(),
                    )
                    .await
                {
                    tracing::warn!(
                        server = %server_name,
                        error = %e,
                        "failed to send permission request"
                    );
                }
            }
        }

        // Wait for response with 5 minute timeout
        let result = tokio::time::timeout(Duration::from_secs(300), rx).await;

        // Clean up pending
        {
            let mut pending = self.state.pending_permissions.lock();
            pending.remove(&request_id);
        }

        match result {
            Ok(Ok(resp)) => {
                if resp.approved {
                    InteractionResponse::Decisions(
                        items
                            .iter()
                            .map(|_| ApprovalDecision::Approve {
                                source: Some("channel".to_string()),
                            })
                            .collect(),
                    )
                } else {
                    InteractionResponse::Decisions(
                        items
                            .iter()
                            .map(|_| ApprovalDecision::Reject {
                                reason: resp.reason.clone(),
                                source: Some("channel".to_string()),
                            })
                            .collect(),
                    )
                }
            }
            Ok(Err(_)) | Err(_) => InteractionResponse::Decisions(
                items
                    .iter()
                    .map(|_| ApprovalDecision::Reject {
                        reason: "channel permission timeout".to_string(),
                        source: None,
                    })
                    .collect(),
            ),
        }
    }
}
