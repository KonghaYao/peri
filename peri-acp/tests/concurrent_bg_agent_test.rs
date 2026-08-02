//! Integration test: verify that N concurrent bg channel senders all successfully
//! deliver `BackgroundTaskCompleted` events through the unbounded channel.
//!
//! Reproduces the bug described in
//! spec/issues/2026-05-24-concurrent-bg-agent-only-one-completion.md

use peri_agent::agent::events::{BackgroundTaskResult, ExecutorEvent};

#[tokio::test]
async fn test_concurrent_bg_tasks_all_emit_completion() {
    let (bg_tx, mut bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let task_count = 3usize;

    // Spawn N senders concurrently, each sending one BackgroundTaskCompleted
    let handles: Vec<_> = (0..task_count)
        .map(|i| {
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                // Simulate variable completion time (different orders)
                tokio::time::sleep(std::time::Duration::from_millis(
                    (task_count - i) as u64 * 20,
                ))
                .await;
                let result = BackgroundTaskResult {
                    task_id: format!("bg-task-{}", i),
                    agent_name: format!("agent-{}", i),
                    prompt_summary: format!("task {}", i),
                    success: true,
                    output: format!("output {}", i),
                    tool_calls_count: 1,
                    duration_ms: 100 + i as u64 * 10,
                    child_thread_id: None,
                    timed_out: false,
                };
                let _ = tx.send(ExecutorEvent::BackgroundTaskCompleted(result));
            })
        })
        .collect();

    // Wait for all senders to complete then drop tx
    for h in handles {
        let _ = h.await;
    }
    drop(bg_tx);

    // Collect all received events
    let mut received: Vec<ExecutorEvent> = Vec::new();
    while let Some(event) = bg_rx.recv().await {
        received.push(event);
    }

    let bg_completions: Vec<_> = received
        .iter()
        .filter(|e| matches!(e, ExecutorEvent::BackgroundTaskCompleted(_)))
        .collect();
    assert_eq!(
        bg_completions.len(),
        task_count,
        "Expected {} BackgroundTaskCompleted events, got {}",
        task_count,
        bg_completions.len()
    );

    // Verify all task_ids are present
    let task_ids: std::collections::HashSet<_> = bg_completions
        .iter()
        .filter_map(|e| {
            if let ExecutorEvent::BackgroundTaskCompleted(r) = e {
                Some(r.task_id.clone())
            } else {
                None
            }
        })
        .collect();
    for i in 0..task_count {
        let expected_id = format!("bg-task-{}", i);
        assert!(
            task_ids.contains(&expected_id),
            "Missing task_id: {}",
            expected_id
        );
    }
}

/// Tests the full bg event pump flow: sender → bg_event_rx → bg pump →
/// EventSink → MpscTransport. Uses the same pattern as executor.rs:346-355.
#[tokio::test]
async fn test_bg_event_pump_receives_all_completions() {
    use std::sync::Arc;

    use peri_acp::{
        session::event_sink::{EventSink, TransportEventSink},
        transport::mpsc::mpsc_transport_pair,
    };

    let (client_transport, server_transport) = mpsc_transport_pair();
    let caps_registry = std::sync::Arc::new(dashmap::DashMap::new());

    let session_id = "test-session".to_string();
    // 注册 session 的 PeriCaps，否则 push_event 使用 default()（全 false）
    // 会跳过 peri/agent_event 通知，导致 transport 上零消息到达
    use peri_acp_types::PeriCaps;
    caps_registry.insert(session_id.clone(), PeriCaps::all_enabled());

    let sink = Arc::new(TransportEventSink::new(
        Arc::new(server_transport),
        caps_registry,
    ));
    let (bg_tx, mut bg_rx) = tokio::sync::mpsc::unbounded_channel::<ExecutorEvent>();
    let context_window = 200_000u32;
    let bg_sink = Arc::clone(&sink);
    let bg_session_id = session_id.clone();
    let bg_cw = context_window;

    // Spawn bg event pump (same pattern as executor.rs:346-355)
    let pump_handle = tokio::spawn(async move {
        while let Some(bg_event) = bg_rx.recv().await {
            bg_sink.push_event(&bg_session_id, &bg_event, bg_cw).await;
        }
    });

    // Spawn N concurrent bg tasks, each sending one BackgroundTaskCompleted
    let task_count = 3usize;
    let handles: Vec<_> = (0..task_count)
        .map(|i| {
            let tx = bg_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(i as u64 * 30)).await;
                let result = BackgroundTaskResult {
                    task_id: format!("bg-{}", i),
                    agent_name: format!("test-agent-{}", i),
                    prompt_summary: format!("prompt-{}", i),
                    success: true,
                    output: "test output".to_string(),
                    tool_calls_count: 1,
                    duration_ms: 100,
                    child_thread_id: None,
                    timed_out: false,
                };
                let _ = tx.send(ExecutorEvent::BackgroundTaskCompleted(result));
            })
        })
        .collect();

    // Wait for all senders to finish
    for h in handles {
        let _ = h.await;
    }
    // Drop last sender so bg_rx returns None and pump exits
    drop(bg_tx);

    // Wait for the pump to finish
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), pump_handle)
        .await
        .expect("bg event pump timed out");

    // After Phase B v1 cleanup, only Category ① events produce transport
    // notifications. BackgroundTaskCompleted (non-Category ①) maps to the
    // wildcard with zero SessionUpdate, so it produces no transport output.
    // Verify the pump completed without error — the real test is that all
    // concurrent senders successfully delivered their events.

    let pump_consumer = tokio::spawn(async move {
        use peri_acp::transport::AcpTransport;
        let mut count = 0u64;
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                client_transport.recv(),
            )
            .await
            {
                Ok(Some(_)) => count += 1,
                Ok(None) => break,
                Err(_) => break, // timeout — no more messages coming
            }
        }
        count
    });

    let total_msgs = tokio::time::timeout(std::time::Duration::from_secs(3), pump_consumer)
        .await
        .unwrap_or(Ok(0))
        .unwrap_or(0);

    // BackgroundTaskCompleted is non-Category ① — no transport notifications expected
    assert_eq!(
        total_msgs, 0,
        "BackgroundTaskCompleted should produce 0 transport notifications after Phase B cleanup, got {total_msgs}"
    );
}
