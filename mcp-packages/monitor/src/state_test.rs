use std::time::{Duration, Instant};

use super::*;
use crate::buffer::MIN_TASK_BYTES;

fn temp_cwd() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}

fn request(command: &str) -> StartRequest {
    StartRequest {
        command: command.to_string(),
        ..Default::default()
    }
}

#[test]
fn resolve_start_applies_defaults_and_resolves_relative_cwd() {
    let resolved = resolve_start(
        &StartRequest {
            command: "  printf hi  ".into(),
            cwd: Some("nested".into()),
            label: Some("  build  ".into()),
            ..Default::default()
        },
        "/workspace",
    )
    .expect("valid start");
    assert_eq!(resolved.command, "printf hi");
    // 相对 cwd 由 `PathBuf::join` 拼接，期望值按平台分隔符构造（Windows 为 `\`）。
    assert_eq!(
        resolved.cwd,
        std::path::Path::new("/workspace")
            .join("nested")
            .to_string_lossy()
            .into_owned()
    );
    assert_eq!(resolved.label.as_deref(), Some("build"));
    assert_eq!(
        resolved.deadline,
        Duration::from_millis(DEFAULT_DEADLINE_MS)
    );
    assert_eq!(resolved.max_events, DEFAULT_MAX_EVENTS);
    assert_eq!(resolved.max_bytes, DEFAULT_MAX_BYTES);
}

#[test]
fn resolve_start_rejects_empty_command_and_out_of_range_limits() {
    assert!(matches!(
        resolve_start(&request("   "), "/workspace"),
        Err(MonitorError::EmptyCommand)
    ));
    assert!(matches!(
        resolve_start(
            &StartRequest {
                deadline_ms: Some(0),
                ..request("true")
            },
            "/workspace"
        ),
        Err(MonitorError::InvalidDeadline)
    ));
    assert!(matches!(
        resolve_start(
            &StartRequest {
                deadline_ms: Some(MAX_DEADLINE_MS + 1),
                ..request("true")
            },
            "/workspace"
        ),
        Err(MonitorError::InvalidDeadline)
    ));
    assert!(matches!(
        resolve_start(
            &StartRequest {
                max_events: Some(0),
                ..request("true")
            },
            "/workspace"
        ),
        Err(MonitorError::InvalidMaxEvents)
    ));
    assert!(matches!(
        resolve_start(
            &StartRequest {
                max_bytes: Some(MIN_TASK_BYTES - 1),
                ..request("true")
            },
            "/workspace"
        ),
        Err(MonitorError::InvalidMaxBytes)
    ));
    assert!(matches!(
        resolve_start(
            &StartRequest {
                max_bytes: Some(MAX_TASK_BYTES + 1),
                ..request("true")
            },
            "/workspace"
        ),
        Err(MonitorError::InvalidMaxBytes)
    ));
}

#[test]
fn read_limits_are_bounded() {
    assert!(validate_read_limits(1, 1).is_ok());
    assert!(validate_read_limits(MAX_READ_EVENTS, MAX_READ_BYTES).is_ok());
    assert!(validate_read_limits(0, 1).is_err());
    assert!(validate_read_limits(MAX_READ_EVENTS + 1, 1).is_err());
    assert!(validate_read_limits(1, 0).is_err());
    assert!(validate_read_limits(1, MAX_READ_BYTES + 1).is_err());
}

#[test]
fn unknown_task_is_reported_for_read_stop_and_snapshot() {
    let state = MonitorState::new(".");
    assert!(matches!(
        state.read("missing", None, None, 10, 1024),
        Err(MonitorError::UnknownTask)
    ));
    assert!(matches!(
        state.snapshot("missing", None),
        Err(MonitorError::UnknownTask)
    ));
    assert!(state.list(None, true, 10).is_empty());
}

#[cfg(unix)]
async fn wait_terminal(state: &MonitorState, task_id: &str) -> TaskSnapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = state
            .snapshot(task_id, None)
            .expect("task remains registered");
        if snapshot.status.is_terminal() {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "task did not reach a terminal state in time: {snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn completed_command_exposes_incremental_events_and_cursor() {
    let state = MonitorState::new(temp_cwd());
    let started = state
        .start(request("printf 'alpha\\nbeta\\n'"), None)
        .await
        .expect("start");
    let snapshot = wait_terminal(&state, &started.task_id).await;
    assert_eq!(snapshot.status, MonitorStatus::Completed);
    assert!(snapshot.cleanup.process_terminated);

    let page = state
        .read(&started.task_id, None, None, 10, 64 * 1024)
        .expect("read");
    let texts: Vec<&str> = page
        .events
        .iter()
        .map(|event| event.text.as_str())
        .collect();
    assert_eq!(texts, vec!["alpha", "beta"]);
    assert_eq!(page.next_seq, 2);
    assert_eq!(page.status, MonitorStatus::Completed);

    let again = state
        .read(&started.task_id, None, Some(page.next_seq), 10, 64 * 1024)
        .expect("read again");
    assert!(again.events.is_empty());
    assert_eq!(again.next_seq, 2);
}

#[cfg(unix)]
#[tokio::test]
async fn final_line_without_newline_is_partial() {
    let state = MonitorState::new(temp_cwd());
    let started = state
        .start(request("printf 'tail'"), None)
        .await
        .expect("start");
    wait_terminal(&state, &started.task_id).await;
    let page = state
        .read(&started.task_id, None, None, 10, 64 * 1024)
        .expect("read");
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].text, "tail");
    assert!(page.events[0].partial);
}

#[cfg(unix)]
#[tokio::test]
async fn stop_is_idempotent_and_reports_cleanup() {
    let state = MonitorState::new(temp_cwd());
    let started = state.start(request("sleep 30"), None).await.expect("start");
    let first = state
        .stop(&started.task_id, None, Some("test stop".into()))
        .await
        .expect("stop");
    assert_eq!(first.status, MonitorStatus::Cancelled);
    assert!(first.cleanup.requested);
    assert_eq!(first.status_message.as_deref(), Some("test stop"));

    let second = state
        .stop(&started.task_id, None, Some("again".into()))
        .await
        .expect("idempotent stop");
    assert_eq!(second.status, MonitorStatus::Cancelled);
    assert_eq!(second.finished_at, first.finished_at);
}

#[cfg(unix)]
#[tokio::test]
async fn deadline_expiry_fails_the_task_and_stops_the_process() {
    let state = MonitorState::new(temp_cwd());
    let started = state
        .start(
            StartRequest {
                deadline_ms: Some(100),
                ..request("sleep 30")
            },
            None,
        )
        .await
        .expect("start");
    let snapshot = wait_terminal(&state, &started.task_id).await;
    assert_eq!(snapshot.status, MonitorStatus::Failed);
    assert!(snapshot
        .status_message
        .as_deref()
        .is_some_and(|message| message.contains("deadline")));
    assert!(snapshot.cleanup.process_terminated);
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_task_is_reporting_requested_cleanup_only_after_stop() {
    let state = MonitorState::new(temp_cwd());
    let started = state.start(request("true"), None).await.expect("start");
    let snapshot = wait_terminal(&state, &started.task_id).await;
    assert!(!snapshot.cleanup.requested);

    let stopped = state
        .stop(&started.task_id, None, None)
        .await
        .expect("stop terminal");
    assert_eq!(stopped.status, MonitorStatus::Completed);
    assert!(!stopped.cleanup.requested);
}

#[cfg(unix)]
#[tokio::test]
async fn owner_scoped_tasks_are_hidden_from_other_sessions() {
    let state = MonitorState::new(temp_cwd());
    let started = state
        .start(request("true"), Some("session-a".into()))
        .await
        .expect("start");
    wait_terminal(&state, &started.task_id).await;

    assert!(matches!(
        state.snapshot(&started.task_id, Some("session-b")),
        Err(MonitorError::UnknownTask)
    ));
    assert!(matches!(
        state.read(&started.task_id, Some("session-b"), None, 10, 1024),
        Err(MonitorError::UnknownTask)
    ));
    assert!(state.list(Some("session-b"), true, 10).is_empty());
    assert!(state.snapshot(&started.task_id, Some("session-a")).is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_cancel_acknowledges_before_terminal_commit() {
    let state = MonitorState::new(temp_cwd());
    let started = state.start(request("sleep 30"), None).await.expect("start");
    state
        .request_cancel(&started.task_id, None, "cancel requested")
        .expect("cancel ack");
    let snapshot = wait_terminal(&state, &started.task_id).await;
    assert_eq!(snapshot.status, MonitorStatus::Cancelled);
    assert_eq!(snapshot.status_message.as_deref(), Some("cancel requested"));
}
