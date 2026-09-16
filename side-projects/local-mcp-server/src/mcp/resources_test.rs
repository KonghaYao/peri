//! `mcp::resources` 的单元测试（WP-005）。
//!
//! 覆盖：URI 形状校验（含走私型输入）、所有者过滤、载荷形状，以及订阅轮询的
//! "基线不发通知、变化才发一条、之后不再重复"语义。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{
    collection_payload, collection_resource, is_task_resource_uri, is_valid_task_id,
    owned_snapshot, owned_snapshots, parse_task_uri, spawn_task_update_watcher, status_label,
    task_payload, task_resource, task_uri, TaskResourceUri, TaskStatusSource, TASKS_URI,
    TASK_RESOURCE_MIME_TYPE, TASK_WATCH_INTERVAL,
};
use crate::mcp::identity::ConnectionIdentity;
use crate::wire::{BoxFuture, TaskSnapshot, TaskStatus};

const TASK_ID: &str = "shell-0192f0f0-0000-7000-8000-000000000000";

fn snapshot(owner: &str, instance: &str, status: TaskStatus) -> TaskSnapshot {
    TaskSnapshot {
        task_id: TASK_ID.to_string(),
        owner: owner.to_string(),
        client_instance: instance.to_string(),
        status,
        pid: Some(4242),
        pgid: Some(4242),
        stdout_log: Some("/tmp/local-mcp-ws/.local-mcp/logs/x.out.log".to_string()),
        stderr_log: None,
        exit_code: None,
        started_at: "2026-09-11T12:00:00Z".to_string(),
        ended_at: None,
    }
}

/// 第二次轮询起把状态从 running 翻成 completed 的脚本状态源。
struct FlippingSource {
    polls: AtomicUsize,
}

impl TaskStatusSource for FlippingSource {
    fn snapshots<'a>(
        &'a self,
        principal: &'a str,
        client_instance: &'a str,
    ) -> BoxFuture<'a, Vec<TaskSnapshot>> {
        let snapshot = self.snapshot_sync(principal, client_instance);
        Box::pin(async move { vec![snapshot] })
    }

    fn snapshot<'a>(
        &'a self,
        principal: &'a str,
        client_instance: &'a str,
        task_id: &'a str,
    ) -> BoxFuture<'a, Option<TaskSnapshot>> {
        let owned = task_id == TASK_ID;
        let snapshot = self.snapshot_sync(principal, client_instance);
        Box::pin(async move { owned.then_some(snapshot) })
    }
}

impl FlippingSource {
    fn snapshot_sync(&self, principal: &str, client_instance: &str) -> TaskSnapshot {
        let poll = self.polls.fetch_add(1, Ordering::SeqCst);
        let status = if poll == 0 {
            TaskStatus::Running
        } else {
            TaskStatus::Completed
        };
        snapshot(principal, client_instance, status)
    }
}

#[test]
fn test_parse_task_uri_accepts_only_own_namespace_and_safe_ids() {
    assert_eq!(parse_task_uri(TASKS_URI), Some(TaskResourceUri::Collection));
    assert_eq!(
        parse_task_uri(&task_uri(TASK_ID)),
        Some(TaskResourceUri::Task(TASK_ID.to_string()))
    );
    for rejected in [
        "sandbox://task",
        "sandbox://tasks/",
        "sandbox://tasks/..",
        "sandbox://tasks/a/b",
        "sandbox://tasks/a%2Fb",
        "sandbox://tasks/with space",
        "file:///etc/passwd",
        "tasks",
    ] {
        assert_eq!(parse_task_uri(rejected), None, "{rejected} 不应被解析");
        assert!(!is_task_resource_uri(rejected));
    }
}

#[test]
fn test_task_id_validation_bounds_length_charset_and_traversal_shapes() {
    assert!(is_valid_task_id(
        "shell-0192f0f0-0000-7000-8000-000000000000"
    ));
    assert!(is_valid_task_id("a"));
    assert!(!is_valid_task_id(""));
    assert!(!is_valid_task_id(&"a".repeat(129)));
    assert!(is_valid_task_id(&"a".repeat(128)));
    for rejected in [
        "../etc",
        "..",
        ".",
        ".hidden",
        "-leading-dash",
        "a..b",
        "a b",
        "a/b",
        "a%b",
        "任务",
    ] {
        assert!(!is_valid_task_id(rejected), "{rejected} 不应被接受");
    }
}

#[test]
fn test_owner_filter_drops_foreign_and_mismatched_snapshots() {
    let identity = ConnectionIdentity::new("principal-a", "conn-a");
    let mine = snapshot("principal-a", "conn-a", TaskStatus::Running);
    let foreign_owner = snapshot("principal-b", "conn-a", TaskStatus::Running);
    let foreign_instance = snapshot("principal-a", "conn-b", TaskStatus::Running);

    let kept = owned_snapshots(
        vec![
            mine.clone(),
            foreign_owner.clone(),
            foreign_instance.clone(),
        ],
        &identity,
    );
    assert_eq!(kept, vec![mine.clone()]);
    assert_eq!(owned_snapshot(Some(foreign_owner), &identity), None);
    assert_eq!(owned_snapshot(Some(mine.clone()), &identity), Some(mine));
}

#[test]
fn test_resource_entries_and_payloads_are_json_and_self_describing() {
    let snapshot = snapshot("principal-a", "conn-a", TaskStatus::Completed);
    let collection = collection_resource();
    assert_eq!(collection.uri, TASKS_URI);
    assert_eq!(
        collection.mime_type.as_deref(),
        Some(TASK_RESOURCE_MIME_TYPE)
    );

    let single = task_resource(&snapshot);
    assert_eq!(single.uri, task_uri(TASK_ID));
    assert_eq!(single.mime_type.as_deref(), Some(TASK_RESOURCE_MIME_TYPE));
    assert!(single.description.unwrap_or_default().contains("completed"));
    assert_eq!(status_label(TaskStatus::TimedOut), "timed_out");

    let collection_body: serde_json::Value =
        serde_json::from_str(&collection_payload(std::slice::from_ref(&snapshot))).expect("JSON");
    assert_eq!(collection_body["tasks"][0]["task_id"], TASK_ID);
    assert_eq!(collection_body["tasks"][0]["status"], "completed");

    let single_body: serde_json::Value =
        serde_json::from_str(&task_payload(&snapshot)).expect("JSON");
    assert_eq!(single_body["owner"], "principal-a");
    assert_eq!(single_body["client_instance"], "conn-a");
}

#[tokio::test]
async fn test_watcher_notifies_once_after_state_change_and_repeats_not() {
    let identity = ConnectionIdentity::new("principal-a", "conn-a");
    let source: Arc<dyn TaskStatusSource> = Arc::new(FlippingSource {
        polls: AtomicUsize::new(0),
    });
    let shutdown = CancellationToken::new();
    let mut updates =
        spawn_task_update_watcher(source, identity, vec![task_uri(TASK_ID)], shutdown.clone());

    let first = tokio::time::timeout(TASK_WATCH_INTERVAL * 12, updates.recv())
        .await
        .expect("状态变化必须在轮询窗口内送达");
    assert_eq!(first.as_deref(), Some(task_uri(TASK_ID).as_str()));

    let repeated = tokio::time::timeout(TASK_WATCH_INTERVAL * 3, updates.recv()).await;
    assert!(
        repeated.is_err(),
        "状态未再变化时不得重复发送通知：{repeated:?}"
    );

    shutdown.cancel();
}

#[tokio::test]
async fn test_watcher_drops_snapshots_that_do_not_belong_to_the_connection() {
    // 敌意/有缺陷的 provider：无视调用方身份，永远返回别人的任务快照。
    // 协议层必须用可信身份再次校验（纵深防御），因此不得产生任何通知。
    let source: Arc<dyn TaskStatusSource> = Arc::new(ForeignSource);
    let shutdown = CancellationToken::new();
    let mut updates = spawn_task_update_watcher(
        source,
        ConnectionIdentity::new("principal-b", "conn-b"),
        vec![task_uri(TASK_ID)],
        shutdown.clone(),
    );

    let none = tokio::time::timeout(TASK_WATCH_INTERVAL * 6, updates.recv()).await;
    assert!(none.is_err(), "越权快照不得产生任何通知：{none:?}");
    shutdown.cancel();
}

/// 永远返回他人任务的 provider（模拟越权实现）。
struct ForeignSource;

impl TaskStatusSource for ForeignSource {
    fn snapshots<'a>(
        &'a self,
        _principal: &'a str,
        _client: &'a str,
    ) -> BoxFuture<'a, Vec<TaskSnapshot>> {
        Box::pin(async { Vec::new() })
    }

    fn snapshot<'a>(
        &'a self,
        _principal: &'a str,
        _client: &'a str,
        task_id: &'a str,
    ) -> BoxFuture<'a, Option<TaskSnapshot>> {
        let owned = task_id == TASK_ID;
        Box::pin(async move {
            owned.then(|| {
                snapshot(
                    "principal-someone-else",
                    "conn-someone-else",
                    TaskStatus::Completed,
                )
            })
        })
    }
}
