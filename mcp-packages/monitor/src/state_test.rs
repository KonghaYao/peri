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
fn resolve_start_rejects_oversized_label() {
    let max_label = "x".repeat(MAX_LABEL_BYTES);
    assert!(
        resolve_start(
            &StartRequest {
                label: Some(max_label),
                ..request("true")
            },
            "/workspace"
        )
        .is_ok(),
        "label at the byte limit is accepted"
    );

    let oversized = "x".repeat(MAX_LABEL_BYTES + 1);
    let error = resolve_start(
        &StartRequest {
            label: Some(oversized),
            ..request("true")
        },
        "/workspace",
    )
    .expect_err("oversized label must be rejected");
    assert!(
        matches!(
            error,
            MonitorError::TooLong {
                field: "label",
                max: MAX_LABEL_BYTES
            }
        ),
        "unexpected error: {error}"
    );
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

#[test]
fn invalid_alert_patterns_are_rejected_before_start() {
    assert!(matches!(
        resolve_start(
            &StartRequest {
                alert: Some("(".into()),
                ..request("true")
            },
            "/workspace"
        ),
        Err(MonitorError::InvalidAlert(_))
    ));
    let oversized = "x".repeat(MAX_ALERT_PATTERN_CHARS + 1);
    assert!(matches!(
        resolve_start(
            &StartRequest {
                alert: Some(oversized),
                ..request("true")
            },
            "/workspace"
        ),
        Err(MonitorError::InvalidAlert(_))
    ));
    assert!(resolve_start(
        &StartRequest {
            alert: Some("ERROR|WARN".into()),
            ..request("true")
        },
        "/workspace"
    )
    .expect("valid alert")
    .alert
    .is_some());
}

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

/// A terminal task can still have a truncated first page. The documented loop
/// (continue while `truncated`, stop only when terminal and `truncated` is
/// false) must drain every retained event.
#[cfg(unix)]
#[tokio::test]
async fn terminal_task_still_pages_remaining_events() {
    let state = MonitorState::new(temp_cwd());
    let started = state
        .start(request("seq 1 1000"), None)
        .await
        .expect("start");
    let snapshot = wait_terminal(&state, &started.task_id).await;
    assert_eq!(snapshot.status, MonitorStatus::Completed);

    let first = state
        .read(
            &started.task_id,
            None,
            None,
            DEFAULT_READ_LIMIT,
            DEFAULT_READ_BYTES,
        )
        .expect("first page");
    assert_eq!(first.events.len(), DEFAULT_READ_LIMIT);
    assert!(
        first.truncated,
        "1000 lines must not fit in one 200-event page"
    );
    assert_eq!(first.status, MonitorStatus::Completed);
    assert_eq!(first.next_seq, DEFAULT_READ_LIMIT as u64);

    let mut cursor = first.next_seq;
    let mut total = first.events.len();
    let mut pages = 1;
    let last = loop {
        pages += 1;
        assert!(pages <= 10, "every page must make progress without looping");
        let page = state
            .read(
                &started.task_id,
                None,
                Some(cursor),
                DEFAULT_READ_LIMIT,
                DEFAULT_READ_BYTES,
            )
            .expect("page");
        total += page.events.len();
        cursor = page.next_seq;
        if !page.truncated {
            break page;
        }
    };
    assert_eq!(total, 1000, "the tail must not be lost after terminal");
    assert_eq!(
        last.events.last().map(|event| event.text.as_str()),
        Some("1000")
    );
    assert_eq!(cursor, 1000);
}

#[cfg(unix)]
#[tokio::test]
async fn read_with_a_tiny_byte_budget_returns_one_event_and_progress() {
    let state = MonitorState::new(temp_cwd());
    let started = state
        .start(request("printf 'alpha\\nbeta\\n'"), None)
        .await
        .expect("start");
    wait_terminal(&state, &started.task_id).await;

    let first = state
        .read(&started.task_id, None, None, 10, 1)
        .expect("read");
    assert_eq!(
        first.events.len(),
        1,
        "a too-small max_bytes must still return one complete event"
    );
    assert_eq!(first.events[0].text, "alpha");
    assert!(first.truncated);
    assert_eq!(first.next_seq, 1);

    let second = state
        .read(&started.task_id, None, Some(first.next_seq), 10, 1)
        .expect("read next");
    assert_eq!(second.events[0].text, "beta");
    assert_eq!(second.next_seq, 2);
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
async fn stop_rejects_oversized_reason_without_requesting_cancel() {
    let state = MonitorState::new(temp_cwd());
    let started = state.start(request("sleep 30"), None).await.expect("start");

    let oversized = "x".repeat(MAX_REASON_BYTES + 1);
    let error = state
        .stop(&started.task_id, None, Some(oversized))
        .await
        .expect_err("oversized stop reason must be rejected");
    assert!(
        matches!(
            error,
            MonitorError::TooLong {
                field: "reason",
                max: MAX_REASON_BYTES
            }
        ),
        "unexpected error: {error}"
    );
    let snapshot = state.snapshot(&started.task_id, None).expect("snapshot");
    assert_eq!(snapshot.status, MonitorStatus::Working);
    assert!(!snapshot.cleanup.requested);

    let stopped = state
        .stop(&started.task_id, None, Some("stop for cleanup".into()))
        .await
        .expect("bounded stop");
    assert_eq!(stopped.status, MonitorStatus::Cancelled);
}

#[cfg(unix)]
#[tokio::test]
async fn non_zero_exit_marks_the_task_failed() {
    let state = MonitorState::new(temp_cwd());
    let started = state.start(request("exit 3"), None).await.expect("start");
    let snapshot = wait_terminal(&state, &started.task_id).await;
    assert_eq!(snapshot.status, MonitorStatus::Failed);
    assert!(
        snapshot
            .status_message
            .as_deref()
            .is_some_and(|message| message.contains("status 3")),
        "status message must expose the non-zero exit: {snapshot:?}"
    );
}

#[tokio::test]
async fn spawn_failure_is_reported_without_registering_a_task() {
    let state = MonitorState::new(temp_cwd());
    let missing_cwd = std::env::temp_dir()
        .join(format!("peri-monitor-missing-{}", uuid::Uuid::now_v7()))
        .to_string_lossy()
        .into_owned();
    let error = state
        .start(
            StartRequest {
                cwd: Some(missing_cwd),
                ..request("echo never-runs")
            },
            None,
        )
        .await
        .expect_err("a missing working directory must fail the spawn");
    assert!(
        matches!(error, MonitorError::Spawn(_)),
        "unexpected error: {error}"
    );
    assert!(
        state.list(None, true, 10).is_empty(),
        "a failed spawn must not leave a task record"
    );
}

#[test]
fn snapshot_projects_alert_counters_and_last_seq() {
    let state = MonitorState::new(".");
    let task = MonitorTask {
        id: "monitor-counters".into(),
        owner: Some("session-a".into()),
        label: Some("build".into()),
        alert: None,
        started_at: "2026-11-01T00:00:00Z".into(),
        deadline_at: "2026-11-01T01:00:00Z".into(),
        deadline: Duration::from_secs(3600),
        inner: Mutex::new(TaskInner {
            state: TaskState::Working,
            status_message: None,
            last_updated_at: "2026-11-01T00:00:00Z".into(),
            finished_at: None,
            terminal: false,
            cleanup: CleanupReport {
                requested: false,
                process_terminated: false,
            },
        }),
        buffer: Arc::new(Mutex::new(EventBuffer::new(10, MIN_TASK_BYTES))),
        alerts: Mutex::new(AlertCounters {
            matched: 9,
            last_seq: Some(17),
        }),
        cancel: CancellationToken::new(),
    };
    state.tasks.lock().insert(task.id.clone(), Arc::new(task));

    let snapshot = state
        .snapshot("monitor-counters", Some("session-a"))
        .expect("snapshot");
    assert_eq!(snapshot.alert_matched, 9);
    assert_eq!(snapshot.last_alert_seq, Some(17));
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

#[tokio::test]
async fn alert_match_updates_cumulative_count_and_last_seq_without_event_text() {
    let state = MonitorState::new(temp_cwd());
    let started = state
        .start(
            StartRequest {
                alert: Some("ERROR".into()),
                label: Some("build".into()),
                ..request("echo ERROR one; echo ERROR two")
            },
            Some("session-a".into()),
        )
        .await
        .expect("start");
    wait_terminal(&state, &started.task_id).await;

    let snapshot = state
        .snapshot(&started.task_id, Some("session-a"))
        .expect("snapshot");
    assert_eq!(snapshot.alert_matched, 2);
    assert_eq!(snapshot.last_alert_seq, Some(2));
    assert_eq!(snapshot.label.as_deref(), Some("build"));

    let meta = peri_acp_types::monitor::MonitorAlertMeta {
        alert_matched: snapshot.alert_matched,
        last_alert_seq: snapshot.last_alert_seq,
        label: snapshot.label.clone(),
    };
    let wire = serde_json::to_string(&meta).expect("serialize");
    assert!(
        !wire.contains("ERROR one"),
        "meta must not carry event text"
    );
    assert!(
        !wire.contains("ERROR two"),
        "meta must not carry event text"
    );

    // Foreign owners see neither the task nor its alert counters.
    assert!(matches!(
        state.snapshot(&started.task_id, Some("session-b")),
        Err(MonitorError::UnknownTask)
    ));
}

#[tokio::test]
async fn non_matching_output_only_fills_the_ring_buffer() {
    let state = MonitorState::new(temp_cwd());
    let started = state
        .start(
            StartRequest {
                alert: Some("ERROR".into()),
                ..request("echo ok")
            },
            None,
        )
        .await
        .expect("start");
    let snapshot = wait_terminal(&state, &started.task_id).await;
    assert_eq!(snapshot.event_count, 1);
    assert_eq!(snapshot.alert_matched, 0);
    assert_eq!(snapshot.last_alert_seq, None);
}
