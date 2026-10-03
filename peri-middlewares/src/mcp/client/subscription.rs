use std::{sync::Arc, time::Duration};

use peri_acp_types::event::{BackgroundTaskResult, EventSink};
use peri_acp_types::plugin::McpSubscriptionsConfig;
use peri_acp_types::session::{InboxHandle, MessageKind, MessageSource};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource as CanonicalReminderSource, SystemReminder, TrustedSystemReminder,
    TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use rmcp::{
    model::{DetailedTask, GetTaskParams, ServerNotification, SubscriptionFilter, TaskPayload},
    service::{Peer, RoleClient, Subscription, SubscriptionEnd},
};
use serde_json::json;

use super::{McpClientPool, McpServiceWrapper};

struct ActiveTaskGuard {
    pool: Arc<McpClientPool>,
    session_id: String,
    task_id: String,
}

impl Drop for ActiveTaskGuard {
    fn drop(&mut self) {
        let mut active = self.pool.active_tasks.write();
        if let Some(tasks) = active.get_mut(&self.session_id) {
            tasks.remove(&self.task_id);
            if tasks.is_empty() {
                active.remove(&self.session_id);
            }
        }
    }
}

impl McpClientPool {
    pub fn register_task_event_sink(&self, session_id: &str, sink: Arc<dyn EventSink>) {
        self.task_event_sinks
            .write()
            .insert(session_id.to_owned(), sink);
    }

    pub(crate) async fn emit_task_started(
        &self,
        session_id: &str,
        task_id: &str,
        kind: &str,
        summary: &str,
    ) {
        let sink = self.task_event_sinks.read().get(session_id).cloned();
        if let Some(sink) = sink {
            sink.push_unstable_event(
                session_id,
                "bg-task-started".into(),
                json!({
                    "task_id": task_id, "kind": kind, "summary": summary,
                    "started_at": chrono::Utc::now().to_rfc3339(),
                }),
            )
            .await;
        }
    }

    pub(crate) fn spawn_task_subscription(
        self: &Arc<Self>,
        server: String,
        session_id: String,
        task_id: String,
        is_workspace_shell: bool,
        peer: Peer<RoleClient>,
    ) {
        self.active_tasks
            .write()
            .entry(session_id.clone())
            .or_default()
            .insert(task_id.clone());
        let guard = ActiveTaskGuard {
            pool: Arc::clone(self),
            session_id: session_id.clone(),
            task_id: task_id.clone(),
        };
        let pool = Arc::clone(self);
        let key = crate::mcp::McpTaskKey::TaskStatus {
            server: server.clone(),
            task_id: task_id.clone(),
        };
        let log_server = server.clone();
        if let Err(error) = self.task_spawner.spawn(key, async move {
            let _guard = guard;
            let filter = SubscriptionFilter::builder().task_id(&task_id).build();
            let Ok(mut subscription) = peer.listen(filter).await else {
                tracing::warn!(server = %server, task_id = %task_id, "MCP task subscription failed");
                return;
            };
            // A fast task may finish before the listen acknowledgement. Read
            // once after subscribing; subsequent changes arrive on the stream.
            if let Ok(snapshot) = peer.get_task(GetTaskParams::new(&task_id)).await {
                if snapshot.task.status().is_terminal() {
                    pool.deliver_task_status(&server, &session_id, &snapshot.task, is_workspace_shell).await;
                    return;
                }
            }
            loop {
                match subscription.next().await {
                    Ok(Some(ServerNotification::TaskStatusNotification(update))) => {
                        let task = &update.params.task;
                        if task.task.task_id == task_id && task.status().is_terminal() {
                            pool.deliver_task_status(&server, &session_id, task, is_workspace_shell).await;
                            break;
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        }) {
            tracing::warn!(server = %log_server, error = %error, "MCP task monitor not admitted");
        }
    }

    async fn deliver_task_status(
        &self,
        server: &str,
        session_id: &str,
        task: &DetailedTask,
        is_workspace_shell: bool,
    ) {
        let Some(inbox) = self.session_inboxes.read().get(session_id).cloned() else {
            return;
        };
        let task_id = &task.task.task_id;
        let status = serde_json::to_value(task.status())
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into());
        let shell_result = match &task.payload {
            _ if !is_workspace_shell => None,
            TaskPayload::Completed { result } => {
                result.get("structuredContent").and_then(|value| {
                    serde_json::from_value::<BackgroundTaskResult>(value.clone()).ok()
                })
            }
            _ => None,
        };
        let success = shell_result
            .as_ref()
            .map(|result| result.success)
            .unwrap_or_else(|| {
                matches!(&task.payload, TaskPayload::Completed { result }
                if result.get("isError").and_then(serde_json::Value::as_bool) != Some(true))
            });
        let output = match &task.payload {
            TaskPayload::Completed { result } => {
                let mut text = result
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|content| content.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !is_workspace_shell {
                    if let Some(details) = result.get("structuredContent") {
                        text.push_str("\nTask details: ");
                        text.push_str(&details.to_string());
                    }
                }
                text
            }
            TaskPayload::Failed { error } => error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Task failed")
                .to_owned(),
            _ => task.task.status_message.clone().unwrap_or_default(),
        };
        let body = shell_result
            .as_ref()
            .map(BackgroundTaskResult::to_notification)
            .unwrap_or_else(|| {
                format!(
                    "MCP task {task_id} on {server} finished with status {status}.\n{}",
                    output.chars().take(16_000).collect::<String>()
                )
            });
        let is_shell = is_workspace_shell;
        let sink = self.task_event_sinks.read().get(session_id).cloned();
        if let Some(sink) = sink {
            let duration_ms = shell_result.as_ref().map_or(0, |result| result.duration_ms);
            sink.push_unstable_event(session_id, "bg-task-completed".into(), json!({
                "task_id": task_id, "kind": if is_shell { "shell" } else { "mcp" },
                "success": success, "output_preview": output.chars().take(512).collect::<String>(),
                "duration_ms": duration_ms,
            })).await;
        }
        let reminder = TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: if is_shell {
                    ReminderCategory::Task
                } else {
                    ReminderCategory::ExternalEvent
                },
                source: CanonicalReminderSource(if is_shell { "shell" } else { "mcp" }.into()),
                kind: if success { "completed" } else { "failed" }.into(),
                severity: if success {
                    ReminderSeverity::Info
                } else {
                    ReminderSeverity::Error
                },
                delivery: if is_shell {
                    ReminderDelivery::Configurable
                } else {
                    ReminderDelivery::Required
                },
                audiences: ReminderAudiences(vec![
                    ReminderAudience::Model,
                    ReminderAudience::Tui,
                    ReminderAudience::Automation,
                ]),
                body,
                summary: Some("MCP task completed".into()),
                metadata: json!({"server": server, "task_id": task_id, "status": status}),
            })
            .expect("MCP task reminder mapping must be valid");
        inbox.push_system_reminder(
            MessageKind::Defer,
            if is_shell {
                MessageSource::ShellComplete
            } else {
                MessageSource::DynamicMcpNotification
            },
            reminder,
        );
    }
    // ── subscriptions/listen（2026-07-28 协议）──────────────────────────────

    /// 广播一条订阅通知到所有已注册的会话 inbox。
    ///
    /// 通知以 canonical `Defer + ExternalEvent` reminder 注入，唤醒 idle executor
    ///（agent 随即读资源 / 调工具回复外部消息）。
    fn broadcast_subscription_notification(&self, server: &str, uri: &str, subscription_id: &str) {
        let handles: Vec<InboxHandle> = self.session_inboxes.read().values().cloned().collect();
        if handles.is_empty() {
            tracing::debug!(server = %server, uri = %uri, "订阅通知到达但无注册会话 inbox");
            return;
        }
        let body = format!(
            "MCP 资源已更新：server={server}, uri={uri}, subscription_id={subscription_id}。请查看并处理。"
        );
        let reminder = TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::ExternalEvent,
                source: CanonicalReminderSource("mcp".into()),
                kind: "subscription_resource_updated".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Required,
                audiences: ReminderAudiences(vec![
                    ReminderAudience::Model,
                    ReminderAudience::Tui,
                    ReminderAudience::Automation,
                ]),
                body,
                summary: Some("MCP 订阅资源已更新".into()),
                metadata: json!({
                    "server": server,
                    "uri": uri,
                    "subscription_id": subscription_id,
                }),
            })
            .expect("MCP subscription reminder mapping must be valid");
        for handle in handles {
            handle.push_system_reminder(
                MessageKind::Defer,
                MessageSource::DynamicMcpNotification,
                reminder.clone(),
            );
        }
        tracing::info!(server = %server, uri = %uri, sessions = %self.session_inboxes.read().len(), "订阅通知已广播到会话 inbox");
    }

    /// 订阅流异常中断后的最大重试次数（每次中断独立计算，收到通知即重置）。
    const SUBSCRIPTION_RETRY_LIMIT: usize = 3;
    /// 订阅流异常中断后的退避基准秒数（指数递增：1s/2s/4s）。
    const SUBSCRIPTION_RETRY_BASE_DELAY_SECS: u64 = 1;

    /// git ref 资源通知的 `resources/read` 上界。
    ///
    /// 通知只说明「资源已更新」，正文要回读；上界保证「读了但读不回来」不会挂住消费
    /// 循环——超时按失败走通用提醒回退（事件不丢）。
    const GIT_REF_READ_TIMEOUT: Duration = Duration::from_secs(2);

    /// 通知分派（Git Watch 下沉的宿主消费入口）。
    ///
    /// - 命中内置 `workspace` 的 git ref 资源 ⇒ 回读资源正文，组装**宿主内置**的
    ///   canonical `git_watch` 提醒（D-5：元数据不从 server 取），`Info` 不唤醒；
    /// - 回读失败 / 超时 ⇒ 回退既有通用订阅提醒（事件不丢，只是语义降级）；
    /// - 其余资源 ⇒ 既有路径逐位不变。
    async fn dispatch_resource_updated(&self, server: &str, uri: &str, subscription_id: &str) {
        if !is_git_watch_resource(server, uri) {
            self.broadcast_subscription_notification(server, uri, subscription_id);
            return;
        }
        match self.read_git_ref_body(server, uri).await {
            Some(body) => self.broadcast_git_watch_notification(server, uri, &body),
            None => {
                tracing::warn!(
                    server = %server,
                    uri = %uri,
                    "git ref 资源回读失败/超时，回退通用订阅提醒"
                );
                self.broadcast_subscription_notification(server, uri, subscription_id);
            }
        }
    }

    /// 回读 git ref 资源正文：`resources/read`（带超时）→ 首个文本内容。
    async fn read_git_ref_body(&self, server: &str, uri: &str) -> Option<String> {
        let peer = self
            .get_client(server)
            .and_then(|handle| handle.peer.clone())?;
        match tokio::time::timeout(
            Self::GIT_REF_READ_TIMEOUT,
            self.read_resource_cached(server, uri, &peer),
        )
        .await
        {
            Ok(Ok((result, _ticket))) => first_text_body(&result),
            Ok(Err(error)) => {
                tracing::warn!(server = %server, uri = %uri, error = %error, "git ref 资源读取失败");
                None
            }
            Err(_) => {
                tracing::warn!(server = %server, uri = %uri, "git ref 资源读取超时");
                None
            }
        }
    }

    /// 把宿主内置的 `git_watch` 提醒推到所有注册会话 inbox（`Info`，不唤醒）。
    fn broadcast_git_watch_notification(&self, server: &str, uri: &str, body: &str) {
        let handles: Vec<InboxHandle> = self.session_inboxes.read().values().cloned().collect();
        if handles.is_empty() {
            tracing::debug!(server = %server, uri = %uri, "git ref 变化到达但无注册会话 inbox");
            return;
        }
        let (kind, reminder) = git_watch_reminder_from_resource(uri, body);
        for handle in handles {
            handle.push_system_reminder(
                kind,
                MessageSource::DynamicMcpNotification,
                reminder.clone(),
            );
        }
        tracing::info!(server = %server, uri = %uri, sessions = %self.session_inboxes.read().len(), "git ref 变化已注入会话");
    }

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
        let _admission = self.lifecycle_registration.lock();
        if !self.is_open() {
            return;
        }
        let pool = Arc::clone(self);
        let task_server = server.to_string();
        let key = crate::mcp::McpTaskKey::Subscription(server.to_string());
        let _ = self.task_spawner.spawn(key, async move {
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
                        pool.dispatch_resource_updated(&task_server, &notif.params.uri, &sid)
                            .await;
                    }
                    Ok(Some(ServerNotification::ResourceListChangedNotification(_))) => {
                        retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
                        pool.invalidate_resource_cache(&task_server, None).await;
                    }
                    Ok(Some(ServerNotification::ToolListChangedNotification(_))) => {
                        retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
                        // rmcp peer 已失效其连接内工具缓存；同步失效跨进程磁盘
                        // tools/list 缓存，使下次回源刷新 bridge 使用的 schema。
                        pool.invalidate_tools_cache(&task_server).await;
                    }
                    Ok(Some(ServerNotification::PromptListChangedNotification(_))) => {
                        // rmcp peer 已失效其连接内 prompt 缓存；本客户端未持久化
                        // prompts，无需磁盘失效。
                        retries_left = Self::SUBSCRIPTION_RETRY_LIMIT;
                    }
                    Ok(Some(_)) => {
                        // 其余通知（Cancelled/Progress/Logging/Task/Custom 等）无需处理
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
            let services = self.services.lock();
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

#[cfg(test)]
mod task_projection_tests {
    use std::sync::Arc;

    use peri_acp_types::{
        event::{BackgroundTaskResult, EventSink, ExecutorEvent, ShellOutput},
        mcp::McpSubscriptionPort,
        session::{MessageQueue, MessageSource, QueuedPayload, SessionInbox},
    };
    use rmcp::model::{DetailedTask, Task, TaskPayload, TaskStatus};
    use serde_json::{json, Value};

    use super::McpClientPool;

    #[derive(Default)]
    struct CapturingSink(parking_lot::Mutex<Vec<(String, Value)>>);

    #[async_trait::async_trait]
    impl EventSink for CapturingSink {
        async fn push_event(&self, _: &str, _: &ExecutorEvent, _: u32) {}
        async fn push_done(&self, _: &str, _: &str, _: Option<&str>) {}
        async fn push_unstable_event(&self, _: &str, event: String, data: Value) {
            self.0.lock().push((event, data));
        }
    }

    #[tokio::test]
    async fn workspace_shell_task_has_task_events_and_file_reference_reminder() {
        let pool = McpClientPool::new_pending();
        let queue = Arc::new(MessageQueue::new());
        let inbox = SessionInbox::new(queue);
        pool.register_inbox("session", inbox.handle());
        let sink = Arc::new(CapturingSink::default());
        pool.register_task_event_sink("session", sink.clone());
        pool.emit_task_started("session", "shell-1", "shell", "sleep 1")
            .await;

        let result = BackgroundTaskResult {
            task_id: "shell-1".into(),
            agent_name: "bg-shell".into(),
            prompt_summary: "sleep 1".into(),
            success: true,
            output: "Shell command completed; read the output files as needed.".into(),
            tool_calls_count: 0,
            duration_ms: 1000,
            child_thread_id: None,
            timed_out: false,
            subagent_failure: None,
            shell_output: Some(Box::new(ShellOutput {
                stdout_path: Some("/tmp/stdout.log".into()),
                stderr_path: Some("/tmp/stderr.log".into()),
                complete: true,
                error: None,
                exit_code: Some(0),
            })),
        };
        let content = json!({
            "content": [{"type": "text", "text": "Background shell command completed.\nShell command completed; read the output files as needed."}],
            "isError": false, "structuredContent": result,
        }).as_object().unwrap().clone();
        let task = DetailedTask::new(
            Task::new(
                "shell-1",
                TaskStatus::Completed,
                "2026-10-03T00:00:00Z",
                "2026-10-03T00:00:01Z",
            ),
            TaskPayload::Completed { result: content },
        );
        pool.deliver_task_status("workspace", "session", &task, true)
            .await;

        let events = sink.0.lock();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "bg-task-started");
        assert_eq!(events[0].1["kind"], "shell");
        assert_eq!(events[1].0, "bg-task-completed");
        assert_eq!(events[1].1["success"], true);
        let messages = inbox.queue().drain_all();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].source, MessageSource::ShellComplete);
        let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
            panic!("reminder")
        };
        let body = &reminder.as_reminder().body;
        assert!(body.contains("/tmp/stdout.log"));
        assert!(!body.contains("Task details:"));
        assert!(!body.contains("structuredContent"));

        // An external server may return the same JSON shape, but it cannot
        // acquire builtin shell reminder semantics from that payload.
        drop(events);
        pool.deliver_task_status("external", "session", &task, false)
            .await;
        let messages = inbox.queue().drain_all();
        assert_eq!(messages[0].source, MessageSource::DynamicMcpNotification);
        let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
            panic!("reminder")
        };
        assert!(reminder.as_reminder().body.contains("MCP task shell-1"));
        assert!(reminder.as_reminder().body.contains("Task details:"));
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

/// 该通知是否属于**宿主内置**的 git ref 资源（D-5 的绑定面）。
///
/// 判定不写第二份实例名字面量：以「实例有默认订阅且默认订阅覆盖该 URI」为准
/// （默认订阅的唯一声明在 `mcp::builtin::workspace_subscription`）。
pub(crate) fn is_git_watch_resource(server: &str, uri: &str) -> bool {
    uri == peri_mcp_workspace::GIT_REF_RESOURCE_URI
        && crate::mcp::builtin::default_subscriptions_for(server)
            .is_some_and(|sub| sub.resources.iter().any(|resource| resource == uri))
}

/// 读回正文的信任边界（§6 风险 6：读回的 payload 不可信，限长、不进控制状态）。
const GIT_REF_BODY_MAX_BYTES: usize = 8 * 1024;

/// 资源正文 → canonical `git_watch` 提醒（**纯函数**，便于单测）。
///
/// 元数据逐字段 = 旧 `GitWatchMiddleware` 的契约（计划 §1 逐字保持项）：category /
/// source / kind / severity / delivery / audiences / summary / metadata 全部写死，
/// `body` = 资源正文逐字（仅超长时按 UTF-8 边界截断）。
///
/// 返回 `MessageKind::Info`（**不唤醒**）：git ref 变化是诊断信息，不是需要 agent 立即
/// 响应的外部事件（旧实现的 `MessageKind::Info` 语义逐字保持）。
pub(crate) fn git_watch_reminder_from_resource(
    uri: &str,
    body: &str,
) -> (MessageKind, TrustedSystemReminder) {
    debug_assert_eq!(
        uri,
        peri_mcp_workspace::GIT_REF_RESOURCE_URI,
        "git_watch 映射只对 git ref 资源生效"
    );
    let reminder = TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Diagnostic,
            source: CanonicalReminderSource("git_watch".into()),
            kind: "repository_ref_changed".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![
                ReminderAudience::Model,
                ReminderAudience::Tui,
                ReminderAudience::Diagnostics,
            ]),
            body: truncate_body(body),
            summary: Some("Git branch 或 HEAD 已变化".into()),
            metadata: json!({}),
        })
        .expect("git watch reminder mapping must be valid");
    (MessageKind::Info, reminder)
}

/// 超长正文按 UTF-8 边界截断（截断处追加标记，避免把截断伪装成完整正文）。
fn truncate_body(body: &str) -> String {
    if body.len() <= GIT_REF_BODY_MAX_BYTES {
        return body.to_string();
    }
    let mut end = GIT_REF_BODY_MAX_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…（资源正文超长，已截断）", &body[..end])
}

/// `resources/read` 结果里的首个文本内容（Blob / 空内容返回 `None`）。
fn first_text_body(result: &rmcp::model::ReadResourceResult) -> Option<String> {
    result.contents.iter().find_map(|content| match content {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
        rmcp::model::ResourceContents::BlobResourceContents { .. } => None,
        // `ResourceContents` 是 non_exhaustive（未来新增形态安全退化，不 panic）。
        _ => None,
    })
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

#[cfg(test)]
#[path = "subscription_test.rs"]
mod tests;
