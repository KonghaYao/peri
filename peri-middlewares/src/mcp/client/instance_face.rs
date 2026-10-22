//! 宿主面「按实例消费」：`tools/call`（含 request `_meta`）、`resources/read`
//! 与 `notifications/resources/updated` 订阅。
//!
//! 端口契约与默认「不可用」语义见 `peri_acp_types::ports::McpPoolPort`；本文件是
//! `McpClientPool` 的具体实现。边界：
//! - **按实例寻址**：实例名解析到 per-session 池内的连接句柄（`get_client`），
//!   实例连接不跨池（会话隔离由池装配保证）；未知实例 / 未连接给出既有文本。
//! - **复用既有映射**：工具结果文本走模型面同一 `format_contents`；错误复用
//!   `ToolCallError` / `ResourceError` 文本，不新造映射。
//! - **直连读取**：宿主面 `resources/read` 不经跨进程资源缓存（缓存提交属资源
//!   工具面的内容验证路径），按需取实例当前正文。
//! - **订阅是唤醒信号**（design `workflow-mcp` §4.3）：通知不携带正文、缓冲满即
//!   丢弃；消费方收到后经 [`McpClientPool::read_instance_resource`] 补读。

use super::*;
use crate::mcp::resource_tool::{ResourceError, RESOURCE_READ_TIMEOUT};
use crate::mcp::tool_bridge::{format_contents, TOOL_CALL_TIMEOUT};
use crate::mcp::ToolCallError;
use peri_acp_types::ports::McpResourceSubscription;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ReadResourceRequestParams, RequestMetaObject,
    ResourceContents, ServerNotification, SubscriptionFilter,
};
use rmcp::service::ServiceError;
use tokio::sync::mpsc;

/// 宿主面订阅的通知缓冲；满即丢弃（唤醒信号，消费方按快照补读）。
const INSTANCE_SUBSCRIPTION_BUFFER: usize = 64;

impl McpClientPool {
    /// 解析池内实例的可消费句柄（按实例消费的统一入口）。
    ///
    /// 未知实例 / 未连接 / 无 peer 都是调用方按错误处理的明确事实：不返回空结果、
    /// 不回落其它实例。句柄保持 `Arc`（连接换代时旧句柄的 peer 由关闭路径收口）。
    fn instance_handle(&self, instance: &str) -> Result<Arc<McpClientHandle>, String> {
        let handle = self.get_client(instance).ok_or_else(|| {
            ResourceError::ServerNotFound {
                server: instance.to_string(),
            }
            .to_string()
        })?;
        if !matches!(handle.status, ClientStatus::Connected) || handle.peer.is_none() {
            let status = if matches!(handle.status, ClientStatus::Connected) {
                ClientStatus::Disconnected
            } else {
                handle.status.clone()
            };
            return Err(ResourceError::NotConnected {
                server: instance.to_string(),
                status,
            }
            .to_string());
        }
        Ok(handle)
    }

    /// 对池内某实例执行 `tools/call`（含 request `_meta`）。
    ///
    /// 复用模型面的请求路径（`tool_request::call_tool`：期限 + 取消归属）与结果
    /// 映射（`format_contents`）；工具级失败（`isError`）返回服务端错误文本原样，
    /// 便于调用方（ACP `workflow/*`）按业务语义映射。
    pub(crate) async fn call_instance_tool(
        &self,
        instance: &str,
        tool: &str,
        arguments: serde_json::Value,
        request_meta: Option<serde_json::Value>,
    ) -> Result<String, String> {
        let handle = self.instance_handle(instance)?;
        let _permit = handle.call_gate.try_acquire().map_err(str::to_owned)?;
        let arguments = match arguments {
            serde_json::Value::Object(map) => map,
            serde_json::Value::Null => Default::default(),
            _ => {
                return Err(ToolCallError::CallFailed {
                    server: instance.to_string(),
                    tool: tool.to_string(),
                    reason: "tool arguments must be a JSON object".into(),
                }
                .to_string());
            }
        };
        let mut request = CallToolRequestParams::new(tool.to_string()).with_arguments(arguments);
        if let Some(meta) = request_meta {
            let map = meta.as_object().ok_or_else(|| {
                ToolCallError::CallFailed {
                    server: instance.to_string(),
                    tool: tool.to_string(),
                    reason: "request _meta must be a JSON object".into(),
                }
                .to_string()
            })?;
            request.meta = Some(RequestMetaObject::from(map.clone()));
        }
        let peer = handle
            .peer
            .as_ref()
            .expect("instance_handle 已确认 peer 存在");
        match crate::mcp::tool_request::call_tool(peer, request, Some(TOOL_CALL_TIMEOUT)).await {
            Ok(CallToolResponse::Complete(result)) => {
                let text = format_contents(&result.content);
                if result.is_error == Some(true) {
                    Err(text)
                } else {
                    Ok(text)
                }
            }
            Ok(_) => Err(ToolCallError::CallFailed {
                server: instance.to_string(),
                tool: tool.to_string(),
                reason: "non-complete tool responses (input-required / task) are not admitted on \
                         the host consumption face"
                    .into(),
            }
            .to_string()),
            Err(error) => Err(instance_tool_error(instance, tool, error)),
        }
    }

    /// 读取池内某实例的资源 URI 正文（首个非空文本内容的按序拼接）。
    pub(crate) async fn read_instance_resource(
        &self,
        instance: &str,
        uri: &str,
    ) -> Result<String, String> {
        let handle = self.instance_handle(instance)?;
        let _permit = handle.call_gate.try_acquire().map_err(str::to_owned)?;
        let peer = handle
            .peer
            .as_ref()
            .expect("instance_handle 已确认 peer 存在");
        let response = peri_time::timeout(
            RESOURCE_READ_TIMEOUT,
            peer.read_resource(ReadResourceRequestParams::new(uri)),
        )
        .await
        .map_err(|_| {
            ResourceError::ReadFailed {
                server: instance.to_string(),
                reason: format!(
                    "resources/read timed out after {}s",
                    RESOURCE_READ_TIMEOUT.as_secs()
                ),
            }
            .to_string()
        })?
        .map_err(|error| {
            ResourceError::ReadFailed {
                server: instance.to_string(),
                reason: error.to_string(),
            }
            .to_string()
        })?;
        let mut texts = Vec::new();
        for content in &response.contents {
            match content {
                ResourceContents::TextResourceContents { text, .. } => texts.push(text.clone()),
                ResourceContents::BlobResourceContents { .. } => {
                    return Err(ResourceError::ReadFailed {
                        server: instance.to_string(),
                        reason: format!("resource {uri} carries binary content"),
                    }
                    .to_string());
                }
                _ => {}
            }
        }
        if texts.is_empty() {
            return Err(ResourceError::ReadFailed {
                server: instance.to_string(),
                reason: format!("resource {uri} has no text content"),
            }
            .to_string());
        }
        Ok(texts.join("\n"))
    }

    /// 就池内某实例的指定资源 URI 订阅 `notifications/resources/updated`。
    ///
    /// 每次调用建立一条独立订阅长流（同一实例 / URI 允许多条：取消是异步收口，
    /// 重建路径不与旧订阅竞态）。通知经专用 channel 交给返回句柄；转发任务归池的
    /// 任务作用域（实例移除 / 重连 / 池关闭时收口），句柄 drop / `cancel()` 亦终止。
    pub(crate) async fn subscribe_instance_resource(
        &self,
        instance: &str,
        uri: &str,
    ) -> Result<McpResourceSubscription, String> {
        let handle = self.instance_handle(instance)?;
        let _permit = handle.call_gate.try_acquire().map_err(str::to_owned)?;
        // A24 关闭语义覆盖订阅面（订阅是能力的外部副作用；与 initialize /
        // reconnect 的配置订阅同门）：命中关闭集的 builtin 实例不建立长流。
        if !self.subscription_allowed(instance) {
            return Err(ResourceError::ReadFailed {
                server: instance.to_string(),
                reason: "instance is closed by session policy; no subscription is established"
                    .into(),
            }
            .to_string());
        }
        let peer = handle
            .peer
            .as_ref()
            .expect("instance_handle 已确认 peer 存在");
        let filter = SubscriptionFilter::builder()
            .resource_subscriptions([uri.to_string()])
            .build();
        let subscription = peri_time::timeout(SUBSCRIPTION_LISTEN_TIMEOUT, peer.listen(filter))
            .await
            .map_err(|_| {
                ResourceError::ReadFailed {
                    server: instance.to_string(),
                    reason: format!(
                        "subscription setup timed out after {}s",
                        SUBSCRIPTION_LISTEN_TIMEOUT.as_secs()
                    ),
                }
                .to_string()
            })?
            .map_err(|error| {
                ResourceError::ReadFailed {
                    server: instance.to_string(),
                    reason: error.to_string(),
                }
                .to_string()
            })?;
        let sequence = self
            .next_instance_subscription
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = super::super::McpTaskKey::InstanceStateSubscription {
            instance: instance.to_string(),
            uri: uri.to_string(),
            sequence,
        };
        let (updates, receiver) = mpsc::channel(INSTANCE_SUBSCRIPTION_BUFFER);
        let cancel = tokio_util::sync::CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_uri = uri.to_string();
        self.task_spawner
            .spawn(key, async move {
                forward_instance_resource_updates(subscription, &task_uri, updates, task_cancel)
                    .await;
            })
            .map_err(|error| {
                ResourceError::ReadFailed {
                    server: instance.to_string(),
                    reason: format!("subscription task was not admitted: {error}"),
                }
                .to_string()
            })?;
        Ok(McpResourceSubscription::new(receiver, cancel))
    }
}

/// 订阅转发循环：`notifications/resources/updated` → 宿主 channel。
///
/// 结束条件（任一）：句柄取消 / 通道关闭（宿主 drop）/ 订阅流终止（连接关闭 /
/// 实例移除 / 池关闭）。缓冲满时丢弃通知——唤醒信号不承担可靠投递，消费方按
/// 快照与游标补读（design §4.3）。
async fn forward_instance_resource_updates(
    mut subscription: rmcp::service::Subscription,
    uri: &str,
    updates: mpsc::Sender<String>,
    cancel: tokio_util::sync::CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            item = subscription.next() => match item {
                Ok(Some(ServerNotification::ResourceUpdatedNotification(notification))) => {
                    if notification.params.uri != uri {
                        continue;
                    }
                    match updates.try_send(notification.params.uri) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {}
                        Err(mpsc::error::TrySendError::Closed(_)) => break,
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => break,
            },
        }
    }
}

/// `tools/call` 的协议 / 传输错误 → 既有 `ToolCallError` 文本。
fn instance_tool_error(instance: &str, tool: &str, error: ServiceError) -> String {
    match error {
        ServiceError::Timeout { timeout } => ToolCallError::Timeout {
            server: instance.to_string(),
            tool: tool.to_string(),
            timeout_secs: timeout.as_secs(),
        }
        .to_string(),
        other => ToolCallError::CallFailed {
            server: instance.to_string(),
            tool: tool.to_string(),
            reason: other.to_string(),
        }
        .to_string(),
    }
}
