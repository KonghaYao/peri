//! 任务状态资源与订阅（WP-005）：`sandbox://tasks`。
//!
//! 背景（R-004）：Bash 的公开输入必须保持 `command`/`timeout`/`run_in_background`
//! 三字段，因此"查询状态、读日志、停止任务"**不能**靠在 Bash 上新增输入字段实现。
//! 迁移后的可访问路径是：
//!
//! 1. 普通 Bash 命令（`Read` 日志路径、`ps`、`kill -- -<pgid>`）——由 WP-003/WP-004 提供；
//! 2. 标准 MCP 资源与通知：`resources/list`、`resources/read`、modern
//!    `subscriptions/listen`、legacy `resources/subscribe` +
//!    `notifications/resources/updated`——本模块提供协议面。
//!
//! ## 身份生命周期（冻结要求）
//!
//! - 资源的可见性与可读性只由**可信身份** `(principal, client_instance)` 决定；
//!   `clientInfo` 永不参与。
//! - 快照由 [`TaskStatusSource`]（WP-003 的 registry）提供，本层**再次**校验
//!   [`ConnectionIdentity::owns`]；不一致一律按"不存在"处理，不区分"别人的"与"没有的"。
//! - 订阅在请求取消/连接关闭时回收：modern 由请求生命周期（SDK 的
//!   `SubscriptionContext`）结束，legacy 由 `resources/unsubscribe` 或通知通道关闭
//!   （`Peer::send_notification` 返回错误）结束。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::Resource;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::mcp::identity::ConnectionIdentity;
use crate::wire::{BoxFuture, TaskSnapshot, TaskStatus};

/// 任务集合资源 URI。
pub const TASKS_URI: &str = "sandbox://tasks";

/// 单任务资源 URI 前缀。
pub const TASK_URI_PREFIX: &str = "sandbox://tasks/";

/// 任务资源的内容类型（JSON）。
pub const TASK_RESOURCE_MIME_TYPE: &str = "application/json";

/// 任务状态轮询间隔：状态面（状态、退出码、日志路径）变化的最大观测延迟。
///
/// 轮询而不是事件流是刻意选择：任务 registry 只需实现两个只读查询方法，协议层不必
/// 反向依赖任务实现的事件类型，跨包接口因此保持最小。
pub const TASK_WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// 单次订阅接受的最大资源数（防止一次订阅请求制造无界轮询）。
pub const MAX_RESOURCE_SUBSCRIPTIONS: usize = 64;

/// 任务 id 允许的字符：`[A-Za-z0-9_.-]`，长度 1..=128，必须以字母/数字开头，且不含 `..`。
///
/// 严格校验的原因：任务 id 会拼进 URI 并进入资源查找路径，放行 `/`、`%`、`..` 或
/// 前导 `.` 会引入 URI 走私与路径穿越式的伪造空间。真实任务 id 形如
/// `shell-<UUIDv7>`，天然满足这些约束。
pub fn is_valid_task_id(task_id: &str) -> bool {
    if task_id.is_empty() || task_id.len() > 128 || task_id.contains("..") {
        return false;
    }
    if !task_id
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        return false;
    }
    task_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// 任务状态来源（实现者：WP-003 的任务 registry；集成者 WP-007 负责接线）。
///
/// 实现必须只返回属于给定主体与连接实例的数据；协议层会再次校验快照字段。
/// 两个方法都必须是非阻塞的只读查询：它们会在订阅轮询里被反复调用。
pub trait TaskStatusSource: Send + Sync {
    /// 列出该主体当前可见的任务快照。
    fn snapshots<'a>(
        &'a self,
        principal: &'a str,
        client_instance: &'a str,
    ) -> BoxFuture<'a, Vec<TaskSnapshot>>;

    /// 读取单个任务快照；不属于该主体或不存在时返回 `None`。
    fn snapshot<'a>(
        &'a self,
        principal: &'a str,
        client_instance: &'a str,
        task_id: &'a str,
    ) -> BoxFuture<'a, Option<TaskSnapshot>>;
}

/// 解析后的任务资源 URI。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskResourceUri {
    /// 任务集合：`sandbox://tasks`。
    Collection,
    /// 单个任务：`sandbox://tasks/{task_id}`。
    Task(String),
}

/// 解析任务资源 URI；非本命名空间或形状非法返回 `None`。
pub fn parse_task_uri(uri: &str) -> Option<TaskResourceUri> {
    if uri == TASKS_URI {
        return Some(TaskResourceUri::Collection);
    }
    let task_id = uri.strip_prefix(TASK_URI_PREFIX)?;
    if !is_valid_task_id(task_id) {
        return None;
    }
    Some(TaskResourceUri::Task(task_id.to_string()))
}

/// 是否为可能被接受的订阅 URI（形状校验，不含所有者判定）。
pub fn is_task_resource_uri(uri: &str) -> bool {
    parse_task_uri(uri).is_some()
}

/// 构造单任务资源 URI。
pub fn task_uri(task_id: &str) -> String {
    format!("{TASK_URI_PREFIX}{task_id}")
}

/// 集合资源条目。
///
/// 条目的 `title`/`description` 是**产品身份**的一部分（它们随 `resources/list` 出现在
/// wire 上）：D-003 起本产品是单进程本机 server，因此这里不得再出现"沙箱"一类已作废的
/// 隔离承诺。`sandbox://tasks` 这个 URI 方案是跨包标识符，统一改名由 WP-P5 处理。
pub fn collection_resource() -> Resource {
    Resource::new(TASKS_URI, "tasks")
        .with_title("本机任务")
        .with_description("当前可信主体的本机任务快照集合（JSON）")
        .with_mime_type(TASK_RESOURCE_MIME_TYPE)
}

/// 单任务资源条目。
pub fn task_resource(snapshot: &TaskSnapshot) -> Resource {
    Resource::new(task_uri(&snapshot.task_id), snapshot.task_id.clone())
        .with_description(format!("任务状态：{}", status_label(snapshot.status)))
        .with_mime_type(TASK_RESOURCE_MIME_TYPE)
}

/// 状态的可读标签（仅用于资源描述，不改变 wire 上的 snake_case 值）。
pub fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Killed => "killed",
        TaskStatus::TimedOut => "timed_out",
        TaskStatus::Gone => "gone",
    }
}

/// 只保留确实属于该连接的任务快照（所有者与连接实例都必须匹配）。
pub fn owned_snapshots(
    snapshots: Vec<TaskSnapshot>,
    identity: &ConnectionIdentity,
) -> Vec<TaskSnapshot> {
    snapshots
        .into_iter()
        .filter(|snapshot| identity.owns(snapshot))
        .collect()
}

/// 只保留确实属于该连接的单任务快照。
pub fn owned_snapshot(
    snapshot: Option<TaskSnapshot>,
    identity: &ConnectionIdentity,
) -> Option<TaskSnapshot> {
    snapshot.filter(|snapshot| identity.owns(snapshot))
}

/// 集合资源的载荷（JSON 文本）。
pub fn collection_payload(snapshots: &[TaskSnapshot]) -> String {
    let value = serde_json::json!({ "tasks": snapshots });
    let mut text =
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| String::from("{\"tasks\":[]}"));
    text.push('\n');
    text
}

/// 单任务资源的载荷（JSON 文本）。
pub fn task_payload(snapshot: &TaskSnapshot) -> String {
    let value = serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null);
    let mut text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| String::from("{}"));
    text.push('\n');
    text
}

/// 启动任务状态轮询：被订阅资源的内容变化时向返回的 channel 推送 URI。
///
/// 语义：
/// - 第一次观察到某个 URI 只建立基线，**不**发通知（订阅前的状态不是"变化"）；
/// - 之后内容（状态/退出码/日志路径/结束时间）变化才推送；
/// - 快照消失或不再属于该连接时静默跳过，不泄露"曾经存在过"；
/// - `shutdown` 取消、接收端 drop、通知通道关闭时任务退出。
pub fn spawn_task_update_watcher(
    source: Arc<dyn TaskStatusSource>,
    identity: ConnectionIdentity,
    uris: Vec<String>,
    shutdown: CancellationToken,
) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel(16);
    let principal = identity.principal().to_string();
    let instance = identity.client_instance().to_string();
    tokio::spawn(async move {
        let mut baseline: HashMap<String, String> = HashMap::new();
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(TASK_WATCH_INTERVAL) => {}
            }
            for uri in &uris {
                let Some(TaskResourceUri::Task(task_id)) = parse_task_uri(uri) else {
                    continue;
                };
                let snapshot = source.snapshot(&principal, &instance, &task_id).await;
                let Some(snapshot) = owned_snapshot(snapshot, &identity) else {
                    continue;
                };
                let current = serde_json::to_string(&snapshot).unwrap_or_default();
                match baseline.get(uri) {
                    None => {
                        baseline.insert(uri.clone(), current);
                    }
                    Some(previous) if *previous != current => {
                        baseline.insert(uri.clone(), current);
                        if sender.send(uri.clone()).await.is_err() {
                            return;
                        }
                    }
                    Some(_) => {}
                }
            }
        }
    });
    receiver
}

/// 生产适配器：WP-003 的任务注册表就是任务状态来源。
///
/// 两个方法都只做**只读**查询，并且都把 `(principal, client_instance)` 作为过滤条件：
/// 越权读取与他人任务在结果上不可区分（都返回"没有"），因此猜 task id 无法探测他人任务
/// 是否存在。协议层（[`crate::mcp::server`]）还会用可信身份对返回的快照再校验一次。
impl TaskStatusSource for crate::tasks::TaskRegistry {
    fn snapshots<'a>(
        &'a self,
        principal: &'a str,
        client_instance: &'a str,
    ) -> BoxFuture<'a, Vec<TaskSnapshot>> {
        Box::pin(async move { self.snapshots_for(principal, client_instance) })
    }

    fn snapshot<'a>(
        &'a self,
        principal: &'a str,
        client_instance: &'a str,
        task_id: &'a str,
    ) -> BoxFuture<'a, Option<TaskSnapshot>> {
        Box::pin(async move { self.snapshot_for(principal, client_instance, task_id) })
    }
}

#[cfg(test)]
#[path = "resources_test.rs"]
mod tests;
