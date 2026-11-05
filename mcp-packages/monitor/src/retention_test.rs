use super::*;

#[tokio::test]
async fn maintenance_is_cancelled_and_cannot_extend_instance_lifetime() {
    let state = MonitorState::new(".");
    let tasks = Arc::downgrade(&state.tasks);
    let buffers = Arc::downgrade(&state.buffers);
    let cancelled = state.maintenance_cancel.clone();
    state.start_maintenance();
    drop(state);
    assert!(cancelled.is_cancelled());
    assert!(tasks.upgrade().is_none());
    assert!(buffers.upgrade().is_none());
    tokio::task::yield_now().await;
}

#[cfg(unix)]
#[tokio::test]
async fn expired_terminal_events_are_reclaimed_without_new_calls_or_losing_live_tasks() {
    let mut state = MonitorState::new(std::env::temp_dir().to_string_lossy());
    state.sweep_interval = Duration::from_millis(10);
    let ended = state
        .start(
            StartRequest {
                command: "printf 'one\\ntwo\\n'".into(),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("start terminal task");
    let running = state
        .start(
            StartRequest {
                command: "sleep 30".into(),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("start live task");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !state
        .snapshot(&ended.task_id, None)
        .expect("task exists")
        .status
        .is_terminal()
    {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let reader = state
        .task(&ended.task_id, None)
        .expect("retain a reader Arc");
    let terminal_ttl = state
        .task_ttl_ms(&ended.task_id)
        .expect("finite terminal TTL");
    assert!(terminal_ttl >= TERMINAL_RETENTION.as_millis() as u64);
    assert!(terminal_ttl < (TERMINAL_RETENTION + Duration::from_secs(5)).as_millis() as u64);
    assert_eq!(
        state
            .task_ttl_ms(&running.task_id)
            .expect("finite live TTL"),
        DEFAULT_DEADLINE_MS + TERMINAL_RETENTION.as_millis() as u64
    );
    let first = state
        .read(&ended.task_id, None, None, 1, MAX_READ_BYTES)
        .expect("first page");
    assert_eq!(first.events[0].text, "one");
    assert!(first.truncated);
    let stopped = state
        .stop(&ended.task_id, None, None)
        .await
        .expect("stop terminal");
    assert_eq!(stopped.event_count, 2, "stop preserves unread event data");
    let second = state
        .read(
            &ended.task_id,
            None,
            Some(first.next_seq),
            1,
            MAX_READ_BYTES,
        )
        .expect("second page");
    assert_eq!(second.events[0].text, "two");
    assert!(state.buffers.resident_bytes() > 0);
    reader.inner.lock().finished_at_monotonic = Some(Instant::now() - TERMINAL_RETENTION);
    while state.tasks.lock().contains_key(&ended.task_id) {
        assert!(
            Instant::now() < deadline,
            "background sweep must expire without another monitor call"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(matches!(
        state.read(&ended.task_id, None, None, 1, MAX_READ_BYTES),
        Err(MonitorError::UnknownTask)
    ));
    assert_eq!(
        reader.buffer.lock().resident_bytes(),
        0,
        "reader Arc cannot retain expired payload"
    );
    assert_eq!(state.buffers.resident_bytes(), 0);
    assert_eq!(
        state
            .snapshot(&running.task_id, None)
            .expect("active task survives")
            .status,
        MonitorStatus::Working
    );
    state
        .stop(&running.task_id, None, None)
        .await
        .expect("stop live producer");
}

#[cfg(unix)]
#[tokio::test]
async fn eviction_does_not_reset_alert_aggregation() {
    let mut state = MonitorState::new(std::env::temp_dir().to_string_lossy());
    state.buffers = Arc::new(BufferBudget::new(16 * 1024));
    let started = state
        .start(
            StartRequest {
                command: "sleep 30".into(),
                alert: Some("ERROR".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("start");
    let task = state.task(&started.task_id, None).expect("task");
    for _ in 0..20 {
        task.admit_event(crate::buffer::CapturedEvent {
            ts: "2026-11-04T00:00:00Z".into(),
            stream: "stdout",
            kind: "line",
            text: format!("ERROR {}", "x".repeat(8192)),
            bytes: 8198,
            truncated: false,
            partial: false,
        });
    }
    let snapshot = task.snapshot();
    assert_eq!(snapshot.alert_matched, 20);
    assert_eq!(snapshot.last_alert_seq, Some(20));
    assert!(snapshot.dropped > 0);
    assert!(state.buffers.resident_bytes() <= 16 * 1024);
    state
        .stop(&started.task_id, None, None)
        .await
        .expect("stop");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_start_admission_cannot_exceed_the_task_limit() {
    let state = Arc::new(MonitorState::new(std::env::temp_dir().to_string_lossy()));
    let started = state
        .start(
            StartRequest {
                command: "sleep 30".into(),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("initial live task");
    let task = state.task(&started.task_id, None).expect("task");
    for index in 1..MAX_RETAINED_TASKS - 1 {
        state
            .tasks
            .lock()
            .insert(format!("existing-{index}"), task.clone());
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut starts = Vec::new();
    for _ in 0..8 {
        let state = state.clone();
        let barrier = barrier.clone();
        starts.push(tokio::spawn(async move {
            barrier.wait().await;
            state
                .start(
                    StartRequest {
                        command: "sleep 30".into(),
                        ..Default::default()
                    },
                    None,
                )
                .await
        }));
    }
    let mut admitted = Vec::new();
    for start in starts {
        match start.await.expect("start task") {
            Ok(started) => admitted.push(started.task_id),
            Err(MonitorError::CapacityReached) => {}
            Err(error) => panic!("unexpected start error: {error}"),
        }
    }
    assert_eq!(admitted.len(), 1);
    assert_eq!(state.tasks.lock().len(), MAX_RETAINED_TASKS);
    for id in admitted.into_iter().chain(std::iter::once(started.task_id)) {
        state
            .stop(&id, None, None)
            .await
            .expect("stop actual producers");
    }
}
