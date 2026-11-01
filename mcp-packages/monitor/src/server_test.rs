use super::*;
use crate::state::CleanupReport;
use rmcp::model::TaskStatus;

fn snapshot(status: MonitorStatus, message: &str) -> TaskSnapshot {
    TaskSnapshot {
        task_id: "monitor-1".into(),
        label: Some("build".into()),
        status,
        status_message: Some(message.into()),
        started_at: "2026-11-01T00:00:00Z".into(),
        last_updated_at: "2026-11-01T00:00:01Z".into(),
        deadline_at: "2026-11-01T01:00:00Z".into(),
        finished_at: status.is_terminal().then(|| "2026-11-01T00:00:01Z".into()),
        event_count: 2,
        dropped: 0,
        pending_wakes: 0,
        wake_dropped: 0,
        alert_matched: 0,
        cleanup: CleanupReport {
            requested: false,
            process_terminated: status.is_terminal(),
        },
    }
}

#[test]
fn server_advertises_tasks_extension_and_tools() {
    let server = MonitorMcpServer::new(".");
    let info = server.get_info();
    assert!(info.capabilities.supports_tasks());
    assert!(info.capabilities.tools.is_some());
}

#[test]
fn seed_task_carries_deadline_metadata() {
    let result = seed_task(crate::state::StartedMonitor {
        task_id: "monitor-1".into(),
        status: "working",
        started_at: "2026-11-01T00:00:00Z".into(),
        deadline_at: "2026-11-01T01:00:00Z".into(),
        label: Some("build".into()),
    });
    assert_eq!(result.task.status, TaskStatus::Working);
    assert_eq!(result.task.task_id, "monitor-1");
    let metadata = result.meta.expect("monitor metadata");
    assert_eq!(
        metadata
            .0
            .get("peri/monitor")
            .and_then(|value| value.get("deadlineAt"))
            .and_then(|value| value.as_str()),
        Some("2026-11-01T01:00:00Z")
    );
}

#[test]
fn detailed_task_maps_terminal_statuses_and_payloads() {
    let completed = detailed_task(snapshot(MonitorStatus::Completed, "Command exited."));
    assert_eq!(completed.task.status, TaskStatus::Completed);
    assert!(matches!(completed.payload, TaskPayload::Completed { .. }));

    let failed = detailed_task(snapshot(MonitorStatus::Failed, "Command failed."));
    assert_eq!(failed.task.status, TaskStatus::Failed);
    assert!(matches!(failed.payload, TaskPayload::Failed { .. }));

    let cancelled = detailed_task(snapshot(MonitorStatus::Cancelled, "Stopped."));
    assert_eq!(cancelled.task.status, TaskStatus::Cancelled);
    assert!(matches!(cancelled.payload, TaskPayload::Cancelled));

    let working = detailed_task(snapshot(MonitorStatus::Working, "Running."));
    assert_eq!(working.task.status, TaskStatus::Working);
    assert!(matches!(working.payload, TaskPayload::Working));
}

#[test]
fn unknown_task_error_is_shared_for_missing_and_foreign_ids() {
    let error = unknown_task(crate::state::MonitorError::UnknownTask);
    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(error.message, "unknown task");
}
