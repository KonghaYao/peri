use super::*;
use crate::agent::async_tasks::durable_task_terminal_delivery;
use crate::session::test_resources::{mock::work::bind_fixture_task, TestSession};
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session_resources::work::WorkQuery;

fn shell_result(task_id: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.into(),
        agent_name: "Bash".into(),
        prompt_summary: "durable shell".into(),
        success: true,
        output: "terminal output".into(),
        tool_calls_count: 0,
        duration_ms: 1200,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

async fn wait_for_ack(callback: &OnBgCompleteFn, result: &BackgroundTaskResult) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if callback(result, BgTaskKind::Shell).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("durable terminal publication never acknowledged");
}

#[tokio::test]
async fn owner_ack_requires_confirmed_reliable_inbox_not_queue_or_transcript() {
    let bound = TestSession::open().await;
    bind_fixture_task(bound.resources(), &bound.thread_id(), 1, "shell-durable").await;
    let queue = MessageQueue::new();
    let delivery =
        durable_task_terminal_delivery(bound.resources(), bound.thread_id(), 1, queue.clone());
    let callback = durable_bg_complete_callback(delivery);
    let result = shell_result("shell-durable");
    assert!(callback(&result, BgTaskKind::Shell).is_err());
    wait_for_ack(&callback, &result).await;
    assert!(callback(&result, BgTaskKind::Shell).is_ok());
    let snapshot = bound
        .resources
        .load_session_work(&WorkQuery {
            session_id: bound.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.state.deliveries.len(), 1);
    assert_eq!(snapshot.state.task_bindings.len(), 1);
    assert!(bound
        .resources
        .load_session_history(&bound.thread_id())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn missing_immutable_task_binding_never_acknowledges_or_falls_back_to_queue() {
    let bound = TestSession::open().await;
    let queue = MessageQueue::new();
    let callback = durable_bg_complete_callback(durable_task_terminal_delivery(
        bound.resources(),
        bound.thread_id(),
        1,
        queue.clone(),
    ));
    let result = shell_result("unbound-shell");
    for _ in 0..10 {
        assert!(callback(&result, BgTaskKind::Shell).is_err());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(queue.is_empty());
    let snapshot = bound
        .resources
        .load_session_work(&WorkQuery {
            session_id: bound.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    assert!(snapshot.state.deliveries.is_empty());
    assert!(bound
        .resources
        .load_session_history(&bound.thread_id())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn same_terminal_identity_cannot_acknowledge_changed_payload() {
    let bound = TestSession::open().await;
    bind_fixture_task(bound.resources(), &bound.thread_id(), 1, "shell-conflict").await;
    let callback = durable_bg_complete_callback(durable_task_terminal_delivery(
        bound.resources(),
        bound.thread_id(),
        1,
        MessageQueue::new(),
    ));
    let mut result = shell_result("shell-conflict");
    assert!(callback(&result, BgTaskKind::Shell).is_err());
    wait_for_ack(&callback, &result).await;
    result.output = "different terminal output".into();
    assert!(callback(&result, BgTaskKind::Shell)
        .unwrap_err()
        .contains("conflicting"));
    let snapshot = bound
        .resources
        .load_session_work(&WorkQuery {
            session_id: bound.thread_id(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.state.deliveries.len(), 1);
}
