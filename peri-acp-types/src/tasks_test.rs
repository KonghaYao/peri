use super::{BgTaskKind, TaskCancelChannel, TaskRecord};
use crate::session::MessageSource;

/// `child_thread_id` 是可选增量字段：旧 JSON（无该键）继续可反序列化，
/// `None` 时不序列化该键，`Some` 时 roundtrip 保真。
#[test]
fn task_record_child_thread_id_is_backward_compatible() {
    let legacy = serde_json::json!({
        "task_id": "bg-01a1407d-c445-7ac3-a7ab-db7f4535cb10",
        "kind": "agent",
        "summary": "identity",
        "started_at": "2026-10-16T00:00:00Z",
        "status": "running",
        "duration_ms": 0,
        "output_preview": null,
    });
    let record: TaskRecord = serde_json::from_value(legacy).unwrap();
    assert!(record.child_thread_id.is_none());
    assert!(serde_json::to_value(&record)
        .unwrap()
        .get("child_thread_id")
        .is_none());

    let mut record = record;
    record.child_thread_id = Some("01a1407d-c445-7ac3-a7ab-db6d6e66266c".into());
    let value = serde_json::to_value(&record).unwrap();
    assert_eq!(
        value["child_thread_id"],
        "01a1407d-c445-7ac3-a7ab-db6d6e66266c"
    );
    let back: TaskRecord = serde_json::from_value(value).unwrap();
    assert_eq!(back.child_thread_id, record.child_thread_id);
}

/// wire 值是持久化与事件消费面的既有可观测契约，改动即破坏兼容。
#[test]
fn bg_task_kind_wire_values_are_stable() {
    for (kind, wire) in [
        (BgTaskKind::Shell, "shell"),
        (BgTaskKind::Agent, "agent"),
        (BgTaskKind::Workflow, "workflow"),
        (BgTaskKind::Mcp, "mcp"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(wire));
    }
}

/// 每个变体的标签、展示名、提醒源、完成来源、排序与取消通道只在此表定义。
#[test]
fn bg_task_kind_mappings_cover_every_variant() {
    let table = [
        (
            BgTaskKind::Shell,
            "shell",
            "bg-shell",
            "shell",
            MessageSource::ShellComplete,
            0,
            TaskCancelChannel::Process,
        ),
        (
            BgTaskKind::Agent,
            "agent",
            "agent",
            "subagent",
            MessageSource::SubAgentComplete,
            1,
            TaskCancelChannel::Closure,
        ),
        (
            BgTaskKind::Workflow,
            "workflow",
            "workflow",
            "workflow",
            MessageSource::WorkflowComplete,
            2,
            TaskCancelChannel::Closure,
        ),
        (
            BgTaskKind::Mcp,
            "mcp",
            "mcp",
            "mcp",
            MessageSource::McpNotification,
            3,
            TaskCancelChannel::External,
        ),
    ];
    for (kind, label, agent_name, reminder_source, source, rank, channel) in table {
        assert_eq!(kind.as_str(), label, "as_str for {kind:?}");
        assert_eq!(
            kind.task_agent_name(),
            agent_name,
            "agent name for {kind:?}"
        );
        assert_eq!(
            kind.reminder_source(),
            reminder_source,
            "reminder source for {kind:?}"
        );
        assert_eq!(
            kind.completion_source(),
            source,
            "completion source for {kind:?}"
        );
        assert_eq!(kind.rank(), rank, "rank for {kind:?}");
        assert_eq!(
            kind.cancel_channel(),
            channel,
            "cancel channel for {kind:?}"
        );
        assert!(
            source.is_background_completion(),
            "completion source for {kind:?} must be recognized by Receive"
        );
    }
}

/// Receive 的终态集合只覆盖后台完成来源，交互/控制类消息不得计入。
#[test]
fn interactive_sources_are_not_background_completions() {
    for source in [
        MessageSource::UserInput,
        MessageSource::GoalSteering,
        MessageSource::TodoSteering,
        MessageSource::CronTrigger,
        MessageSource::StopHookFeedback,
        MessageSource::ChannelMessage,
        MessageSource::SystemInjected,
        MessageSource::HookStopIntent,
        MessageSource::ToolFailureWarning,
        MessageSource::SessionRecovery,
    ] {
        assert!(!source.is_background_completion(), "source {source:?}");
    }
}
