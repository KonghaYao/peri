//! `mcp::identity` 的单元测试（WP-005）。
//!
//! 覆盖点：stdio 身份不可猜且每次连接不同；上下文不携带任何客户端可伪造的字段；
//! 任务所有权判定对 owner 与连接实例都敏感。

use tokio_util::sync::CancellationToken;

use super::ConnectionIdentity;
use crate::wire::{TaskSnapshot, TaskStatus};

fn snapshot(owner: &str, instance: &str) -> TaskSnapshot {
    TaskSnapshot {
        task_id: "shell-0192f0f0-0000-7000-8000-000000000000".to_string(),
        owner: owner.to_string(),
        client_instance: instance.to_string(),
        status: TaskStatus::Running,
        pid: Some(4242),
        pgid: Some(4242),
        stdout_log: Some("/tmp/local-mcp-ws/.local-mcp/logs/x.out.log".to_string()),
        stderr_log: None,
        exit_code: None,
        started_at: "2026-09-11T12:00:00Z".to_string(),
        ended_at: None,
    }
}

#[test]
fn test_stdio_identity_is_unique_per_connection_and_opaque() {
    let first = ConnectionIdentity::stdio();
    let second = ConnectionIdentity::stdio();
    assert_ne!(first.principal(), second.principal());
    assert_ne!(first.client_instance(), second.client_instance());
    assert!(first.principal().starts_with("stdio-principal-"));
    assert!(first.client_instance().starts_with("stdio-conn-"));
    assert!(
        first.principal().len() > "stdio-principal-".len() + 30,
        "主体标识必须包含不可猜的随机部分"
    );
}

#[test]
fn test_identity_owns_only_matching_owner_and_instance() {
    let identity = ConnectionIdentity::new("principal-a", "conn-a");
    assert!(identity.owns(&snapshot("principal-a", "conn-a")));
    assert!(!identity.owns(&snapshot("principal-b", "conn-a")));
    assert!(!identity.owns(&snapshot("principal-a", "conn-b")));
    assert!(!identity.owns(&snapshot("principal-b", "conn-b")));
}

#[test]
fn test_request_context_carries_identity_and_cancellation() {
    let identity = ConnectionIdentity::new("principal-a", "conn-a");
    let cancellation = CancellationToken::new();
    let context = identity.request_context("17", cancellation.clone());
    assert_eq!(context.request_id, "17");
    assert_eq!(context.principal, "principal-a");
    assert_eq!(context.client_instance, "conn-a");
    cancellation.cancel();
    assert!(
        context.cancellation.is_cancelled(),
        "取消信号必须与传输层共享同一个 token"
    );
}
