use super::*;
use crate::kit::{stream_data::TuiTextChunk, tui_render_unit::TuiRenderUnit};
use peri_acp_types::{
    event::ChildEventOrigin,
    identity::AttemptId,
    session::{ExecutionBinding, TurnId},
};

fn run(order: u64) -> ChildEventOrigin {
    ChildEventOrigin {
        child_thread_id: "child".into(),
        execution: ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
        run_order: order,
        sequence: 1,
    }
}
fn start() -> AcpEventData {
    AcpEventData::SubagentStarted {
        agent_id: "child".into(),
        agent_name: "worker".into(),
        is_background: false,
        parent_tool_call_id: None,
    }
}
fn body(text: &str) -> AcpEventData {
    AcpEventData::TextChunk(TuiTextChunk {
        text: text.into(),
        message_id: Some("message".into()),
        agent_id: Some("child".into()),
    })
}
fn stop() -> AcpEventData {
    AcpEventData::SubagentStopped {
        agent_id: "child".into(),
        result: "done".into(),
        is_error: false,
    }
}
async fn send(
    tx: &mpsc::Sender<AcpEventWithEpoch>,
    observed: &mut mpsc::UnboundedReceiver<bool>,
    event: AcpEventData,
    origin: &ChildEventOrigin,
    epoch: u64,
) -> bool {
    tx.send(AcpEventWithEpoch {
        event,
        child_origin: Some(origin.clone()),
        view_epoch: epoch,
        active_session_id: "s1".into(),
    })
    .await
    .unwrap();
    observed.recv().await.unwrap()
}
fn child_text() -> String {
    atoms::VIEW_MODELS
        .state()
        .read()
        .items
        .iter()
        .filter_map(|unit| {
            if let TuiRenderUnit::TuiSubAgentGroup(group) = unit {
                Some(group)
            } else {
                None
            }
        })
        .flat_map(|group| group.view_models.iter())
        .filter_map(|unit| {
            if let TuiRenderUnit::TuiAssistantBubble(bubble) = unit {
                Some(bubble.text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

#[tokio::test]
#[serial]
async fn child_origin_new_run_without_old_stop_and_same_session_reset_are_isolated() {
    let _restore = ReplayAtomsGuard::new();
    let old_active = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = atoms::BRIDGE_RESET_COUNTER.get();
    let old_tasks = atoms::BG_TASKS.state().read().clone();
    let old_display = atoms::BG_DISPLAY.state().read().clone();
    let old_live = atoms::BG_LIVE_DETAIL.state().read().clone();
    let old_identity = atoms::BG_TASK_IDENTITY.state().read().clone();
    let old_agent_ids = atoms::BG_AGENT_IDS.state().read().clone();
    let old_notification = atoms::NOTIFICATION.state().write().take();

    let epoch = old_reset.wrapping_add(1);
    atoms::ACTIVE_SESSION_ID.set("s1".into());
    atoms::BRIDGE_RESET_COUNTER.set(epoch);
    let (tx, rx) = mpsc::channel(32);
    let (observed_tx, mut observed) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let bridge = spawn_acp_bridge_observed(rx, shutdown.clone(), observed_tx);
    let mut old = run(1);
    assert!(send(&tx, &mut observed, start(), &old, epoch).await);
    old.sequence = 2;
    assert!(send(&tx, &mut observed, body("old execution body"), &old, epoch).await);
    let mut current = run(2);
    assert!(send(&tx, &mut observed, start(), &current, epoch).await);
    assert!(!child_text().contains("old execution body"));
    {
        let view = atoms::VIEW_MODELS.state();
        let snapshot = view.read();
        let groups: Vec<_> = snapshot
            .items
            .iter()
            .filter_map(|unit| match unit {
                TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == "child" => Some(group),
                _ => None,
            })
            .collect();
        assert_eq!(
            groups.len(),
            1,
            "unknown old terminal is not rendered as a completed group"
        );
        assert!(groups[0].is_running);
    }

    old.sequence = 3;
    assert!(!send(&tx, &mut observed, stop(), &old, epoch).await);
    current.sequence = 2;
    assert!(send(&tx, &mut observed, body("before reset"), &current, epoch).await);
    let reset_epoch = epoch.wrapping_add(1);
    atoms::BRIDGE_RESET_COUNTER.set(reset_epoch);
    current.sequence = 3;
    assert!(!send(&tx, &mut observed, body("queued old view"), &current, epoch).await);
    current.sequence = 4;
    assert!(
        send(
            &tx,
            &mut observed,
            body("continued current execution"),
            &current,
            reset_epoch
        )
        .await
    );
    current.sequence = 5;
    assert!(send(&tx, &mut observed, stop(), &current, reset_epoch).await);
    let before_duplicate = child_text();
    assert!(before_duplicate.contains("continued current execution"));
    assert!(!before_duplicate.contains("before reset"));
    assert!(!before_duplicate.contains("queued old view"));
    assert!(!send(&tx, &mut observed, stop(), &current, reset_epoch).await);
    assert_eq!(
        child_text(),
        before_duplicate,
        "duplicate terminal cannot erase frozen preview"
    );
    // A stale/retired preview origin cannot veto authoritative task control.
    let task_id = "business-after-preview-expiry";
    assert!(
        send(
            &tx,
            &mut observed,
            AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
                task_id: task_id.into(),
                kind: "agent".into(),
                summary: "control task".into(),
                started_at: String::new(),
                pid: None,
                revision: None,
                status: Some("running".into())
            }),
            &old,
            epoch
        )
        .await
    );
    assert!(
        send(
            &tx,
            &mut observed,
            AcpEventData::BgTaskCompleted {
                task_id: task_id.into(),
                kind: Some("agent".into()),
                success: true,
                duration_ms: 1,
                output_preview: Some("authoritative result".into()),
                revision: None,
            },
            &old,
            epoch
        )
        .await
    );
    assert_eq!(
        atoms::BG_LIVE_DETAIL.state().read()[task_id].status,
        atoms::BgLiveStatus::Succeeded
    );
    assert!(
        !atoms::BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == task_id)
    );
    shutdown.cancel();
    bridge.await.unwrap();
    atoms::ACTIVE_SESSION_ID.set(old_active);
    atoms::BRIDGE_RESET_COUNTER.set(old_reset);
    atoms::BG_TASKS.set(old_tasks);
    atoms::BG_DISPLAY.set(old_display);
    atoms::BG_LIVE_DETAIL.set(old_live);
    atoms::BG_TASK_IDENTITY.set(old_identity);
    atoms::BG_AGENT_IDS.set(old_agent_ids);
    atoms::NOTIFICATION.set(old_notification);
}
