//! `mcp::client::subscription` 的映射与判定纯函数单测（T7）。
//!
//! 断言口径 = 计划 §1「逐字保持项」的提醒元数据契约（旧 `GitWatchMiddleware`）：
//! category=Diagnostic / source=`git_watch` / kind=`repository_ref_changed` /
//! severity=Info / delivery=Configurable / audiences=[Model,Tui,Diagnostics] /
//! summary / metadata={} / **Info 不唤醒**；正文 = 资源正文（超长按 UTF-8 边界截断）。

use super::{
    git_watch_reminder_from_resource, is_git_watch_resource, notification_message_kind,
    McpClientPool,
};
use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::mcp::{McpNotificationMessageKind, MCP_MESSAGE_KIND_META_KEY};
use peri_acp_types::session::{MessageKind, MessageQueue, SessionInbox};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderCategory, ReminderDelivery, ReminderSeverity,
};
use rmcp::model::NotificationMetaObject;
use std::sync::Arc;

const GIT_URI: &str = "workspace://git/ref";

#[tokio::test]
async fn server_notification_kind_controls_resource_update_wake() {
    let pool = McpClientPool::new_pending();
    let inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.register_inbox("session", inbox.handle());
    for (wire, expected) in [("info", MessageKind::Info), ("defer", MessageKind::Defer)] {
        let mut meta = NotificationMetaObject::new();
        meta.insert(MCP_MESSAGE_KIND_META_KEY.into(), wire.into());
        let declared = notification_message_kind("server", Some(&meta)).unwrap();
        assert_eq!(MessageKind::from(declared), expected);
        pool.dispatch_resource_updated("server", "resource://x", "subscription", Some(declared))
            .await;
        assert_eq!(inbox.queue().has_wake_up(), expected == MessageKind::Defer);
        assert_eq!(inbox.queue().drain_all()[0].kind, expected);
    }
    assert_eq!(notification_message_kind("server", None), None);
    let mut invalid = NotificationMetaObject::new();
    invalid.insert(MCP_MESSAGE_KIND_META_KEY.into(), "prompt".into());
    assert_eq!(notification_message_kind("server", Some(&invalid)), None);
    pool.dispatch_resource_updated("server", "resource://x", "subscription", None)
        .await;
    assert_eq!(inbox.queue().drain_all()[0].kind, MessageKind::Defer);
    assert_eq!(McpNotificationMessageKind::Info.as_str(), "info");
}

#[test]
fn reminder_matches_old_contract_field_by_field() {
    let body =
        "[Git watch] Repository ref changed since the last sample:\n- HEAD: aaaaaaa → bbbbbbb";
    let (kind, reminder) = git_watch_reminder_from_resource(GIT_URI, body);

    assert_eq!(kind, MessageKind::Info, "旧契约：Info（不唤醒）");
    assert!(!kind.wakes_up(), "Info 不得唤醒会话");

    let reminder = reminder.as_reminder();
    assert_eq!(reminder.category, ReminderCategory::Diagnostic);
    assert_eq!(reminder.source.0, "git_watch");
    assert_eq!(reminder.kind, "repository_ref_changed");
    assert_eq!(reminder.severity, ReminderSeverity::Info);
    assert_eq!(reminder.delivery, ReminderDelivery::Configurable);
    assert_eq!(reminder.audiences.0.len(), 3);
    assert!(reminder.audiences.contains(ReminderAudience::Model));
    assert!(reminder.audiences.contains(ReminderAudience::Tui));
    assert!(reminder.audiences.contains(ReminderAudience::Diagnostics));
    assert_eq!(
        reminder.summary.as_deref(),
        Some("Git branch 或 HEAD 已变化")
    );
    assert_eq!(reminder.metadata, serde_json::json!({}));
    assert_eq!(reminder.body, body, "正文 = 资源正文逐字");
}

#[test]
fn oversized_body_is_truncated_at_char_boundary() {
    // 8 KiB 上限：超出部分丢弃并留下截断标记（信任边界，§6 风险 6）。
    let body = "头".repeat(4 * 1024); // 12 KiB 的多字节正文
    let (_, reminder) = git_watch_reminder_from_resource(GIT_URI, &body);
    let reminder = reminder.as_reminder();

    assert!(
        reminder.body.len() <= 8 * 1024 + "…（资源正文超长，已截断）".len(),
        "截断后长度必须受控：{}",
        reminder.body.len()
    );
    assert!(
        reminder.body.ends_with("…（资源正文超长，已截断）"),
        "截断必须留下标记"
    );
    assert!(
        body.starts_with(&reminder.body[..reminder.body.len() - "…（资源正文超长，已截断）".len()]),
        "截断只掉尾部，不得改写前缀"
    );

    // 未超长时逐字保留（不做任何加工）。
    let short = "[Git watch] Repository ref snapshot:\n- Branch: main\n- HEAD: ccccccc";
    let (_, reminder) = git_watch_reminder_from_resource(GIT_URI, short);
    assert_eq!(reminder.as_reminder().body, short);
}

#[test]
fn git_watch_binding_is_workspace_instance_and_git_ref_uri_only() {
    assert!(
        is_git_watch_resource("workspace", GIT_URI),
        "内置 workspace 的 git ref 资源走宿主内置映射"
    );
    assert!(
        !is_git_watch_resource("workspace", "workspace://other"),
        "同实例的其它 URI 不映射"
    );
    assert!(
        !is_git_watch_resource("web", GIT_URI),
        "其它 builtin 实例不映射"
    );
    assert!(
        !is_git_watch_resource("external-server", GIT_URI),
        "外部 server 不映射（只绑定内置实例）"
    );
}
