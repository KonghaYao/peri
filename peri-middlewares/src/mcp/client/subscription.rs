use std::sync::Arc;

use peri_acp_types::plugin::McpSubscriptionsConfig;
use peri_acp_types::session::InboxHandle;
use rmcp::{
    model::{ServerNotification, SubscriptionFilter},
    service::{Subscription, SubscriptionEnd},
};

use super::{McpClientPool, McpServiceWrapper};

impl McpClientPool {
    // ── subscriptions/listen（2026-07-28 协议）──────────────────────────────

    /// 广播一条订阅通知到所有已注册的会话 inbox。
    ///
    /// 通知以 `<system-reminder><mcp-subscription …/>` Defer 消息注入，
    /// 唤醒 idle executor（agent 随即读资源 / 调工具回复外部消息）。
    fn broadcast_subscription_notification(&self, server: &str, uri: &str, subscription_id: &str) {
        let handles: Vec<InboxHandle> = self.session_inboxes.read().values().cloned().collect();
        if handles.is_empty() {
            tracing::debug!(server = %server, uri = %uri, "订阅通知到达但无注册会话 inbox");
            return;
        }
        let text = format!(
            "<system-reminder><mcp-subscription server=\"{}\" uri=\"{}\" subscription-id=\"{}\">资源已更新，请查看并处理。</mcp-subscription></system-reminder>",
            xml_escape(server),
            xml_escape(uri),
            xml_escape(subscription_id)
        );
        for handle in handles {
            handle.push_defer(
                peri_acp_types::session::MessageSource::ChannelMessage,
                peri_acp_types::messages::BaseMessage::human(text.clone()),
            );
        }
        tracing::info!(server = %server, uri = %uri, sessions = %self.session_inboxes.read().len(), "订阅通知已广播到会话 inbox");
    }

    /// 订阅流异常中断后的最大重试次数（每次中断独立计算，收到通知即重置）。
    const SUBSCRIPTION_RETRY_LIMIT: usize = 3;
    /// 订阅流异常中断后的退避基准秒数（指数递增：1s/2s/4s）。
    const SUBSCRIPTION_RETRY_BASE_DELAY_SECS: u64 = 1;

    /// 启动订阅消费循环：读取 `subscriptions/listen` 流上的通知并广播。
    ///
    /// 循环持有 `Subscription`（drop 即取消订阅）；transport 关闭或流结束
    /// 时自然退出。tool/prompt list_changed 由 rmcp peer 内部自动失效缓存。
    ///
    /// 流异常中断（`SubscriptionEnd::Lagged` / `Abrupt` / 瞬时错误）时按
    /// 指数退避（1s/2s/4s）重新 `Peer::listen` 恢复，最多重试
    /// [`Self::SUBSCRIPTION_RETRY_LIMIT`] 次；期间收到正常通知会重置计数。
    /// 连接关闭、配置移除或重试耗尽后退出循环并告警。
    pub(crate) async fn spawn_subscription_loop(
        self: &Arc<Self>,
        server: &str,
        mut subscription: Subscription,
    ) {
        let pool = Arc::clone(self);
        let task_server = server.to_string();
        let handle = tokio::spawn(async move {
            // 剩余重试次数：收到通知即重置，保证每段中断序列都有独立恢复机会
            let mut retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
            loop {
                match subscription.next().await {
                    Ok(Some(ServerNotification::ResourceUpdatedNotification(notif))) => {
                        retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
                        let sid = notif
                            .params
                            .meta
                            .as_ref()
                            .and_then(|m| m.subscription_id())
                            .map(|id| id.to_string())
                            .unwrap_or_default();
                        pool.invalidate_resource_cache(&task_server, Some(&notif.params.uri))
                            .await;
                        pool.broadcast_subscription_notification(
                            &task_server,
                            &notif.params.uri,
                            &sid,
                        );
                    }
                    Ok(Some(ServerNotification::ResourceListChangedNotification(_))) => {
                        retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
                        pool.invalidate_resource_cache(&task_server, None).await;
                    }
                    Ok(Some(_)) => {
                        // tool/prompt list_changed：rmcp peer 已失效对应连接内缓存
                        retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
                    }
                    Ok(None) => {
                        // 仅对异常结束（Lagged/Abrupt）重试；Graceful/Cancelled
                        // 为正常终止，不恢复
                        let retriable = matches!(
                            subscription.end(),
                            Some(SubscriptionEnd::Lagged { .. }) | Some(SubscriptionEnd::Abrupt)
                        );
                        if !retriable || retries_left == 0 {
                            tracing::info!(
                                server = %task_server,
                                end = ?subscription.end(),
                                "订阅流结束，停止消费"
                            );
                            break;
                        }
                        // Subscription 为独占对象：重新 listen 前必须 drop 旧
                        // 句柄（drop 自动发送 cancelled 并注销）
                        drop(subscription);
                        match pool
                            .relisten_subscription(&task_server, &mut retries_left)
                            .await
                        {
                            Some(new_subscription) => subscription = new_subscription,
                            None => break,
                        }
                    }
                    Err(e) => {
                        if retries_left == 0 {
                            tracing::warn!(
                                server = %task_server,
                                error = %e,
                                "订阅流错误，重试耗尽，停止消费"
                            );
                            break;
                        }
                        drop(subscription);
                        match pool
                            .relisten_subscription(&task_server, &mut retries_left)
                            .await
                        {
                            Some(new_subscription) => subscription = new_subscription,
                            None => break,
                        }
                    }
                }
            }
        });
        self.subscription_tasks
            .lock()
            .await
            .insert(server.to_string(), vec![handle]);
    }

    /// 订阅流异常中断后的恢复：退避等待后按 server 当前配置重新建立
    /// `subscriptions/listen` 长流。
    ///
    /// 消耗一次重试机会。连接已关闭（services 表无该 server）、订阅配置
    /// 已移除或重新 listen 失败时返回 None 退出循环。
    async fn relisten_subscription(
        self: &Arc<Self>,
        server: &str,
        retries_left: &mut usize,
    ) -> Option<Subscription> {
        *retries_left -= 1;
        let attempt = Self::SUBSCRIPTION_RETRY_LIMIT - *retries_left;
        let delay_secs = Self::SUBSCRIPTION_RETRY_BASE_DELAY_SECS << (attempt - 1);
        tracing::warn!(
            server = %server,
            attempt = %attempt,
            delay_secs = %delay_secs,
            "订阅流异常中断，退避后重新 listen"
        );
        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
        // 连接可能已被移除/重连：取 services 表中的当前 peer
        let peer = {
            let services = self.services.lock().await;
            services.get(server).map(|s| s.peer().clone())
        };
        let Some(peer) = peer else {
            tracing::info!(server = %server, "连接已关闭，订阅循环退出");
            return None;
        };
        // 配置可能已被移除：按当前配置重建过滤器
        let filter = self
            .configs
            .read()
            .get(server)
            .and_then(|c| c.subscriptions.as_ref())
            .filter(|s| !s.is_empty())
            .map(build_subscription_filter);
        let Some(filter) = filter else {
            tracing::info!(server = %server, "订阅配置已移除，订阅循环退出");
            return None;
        };
        match peer.listen(filter).await {
            Ok(new_subscription) => {
                tracing::info!(server = %server, "订阅流重新建立");
                Some(new_subscription)
            }
            Err(e) => {
                tracing::warn!(
                    server = %server,
                    error = %e,
                    "重新 listen 失败，订阅循环退出"
                );
                None
            }
        }
    }
}

/// 由 `McpSubscriptionsConfig` 构建 `subscriptions/listen` 过滤器。
pub(crate) fn build_subscription_filter(sub: &McpSubscriptionsConfig) -> SubscriptionFilter {
    let mut b = SubscriptionFilter::builder();
    if !sub.resources.is_empty() {
        b = b.resource_subscriptions(sub.resources.iter().cloned());
    }
    if sub.tools_list_changed {
        b = b.tools_list_changed();
    }
    if sub.prompts_list_changed {
        b = b.prompts_list_changed();
    }
    if sub.resources_list_changed {
        b = b.resources_list_changed();
    }
    b.build()
}

/// 连接成功后建立 `subscriptions/listen` 长流并启动消费循环（2026-07-28 协议）。
///
/// 失败仅告警——server 可能不支持，连接本身仍可用。initialize / reconnect 共用。
pub(crate) async fn setup_subscription(
    pool: &Arc<McpClientPool>,
    rs: &McpServiceWrapper,
    name: &str,
    sub: &McpSubscriptionsConfig,
) {
    match rs.peer().listen(build_subscription_filter(sub)).await {
        Ok(subscription) => {
            pool.spawn_subscription_loop(name, subscription).await;
            tracing::info!(
                server = %name,
                resources = ?sub.resources,
                "subscriptions/listen 已建立"
            );
        }
        Err(e) => {
            tracing::warn!(
                server = %name,
                error = %e,
                "subscriptions/listen 建立失败（server 可能不支持）"
            );
        }
    }
}

/// XML 转义五个特殊字符（`&` `<` `>` `"` `'`），防止第三方 MCP server
/// 推送的字段值（如资源 URI）注入 `<system-reminder>` 结构。
pub(crate) fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}
