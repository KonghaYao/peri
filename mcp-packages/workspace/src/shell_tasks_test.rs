use std::{sync::Arc, time::Duration};

use rmcp::model::{TaskPayload, TaskStatus};

use super::ShellTasks;
#[test]
fn session_admissions_do_not_require_an_execution_owner() {
    let owner = ShellTasks::new();
    let first = owner.admit("session-a").expect("first request");
    let second = owner.admit("session-a").expect("second request");
    assert_eq!(owner.state.lock().inflight["session-a"], 2);
    drop(first);
    drop(second);
    assert!(!owner.state.lock().inflight.contains_key("session-a"));
}

#[tokio::test]
async fn close_gate_waits_for_admitted_creation_and_rejects_later_work() {
    let owner = ShellTasks::new();
    let admitted = owner.admit("session-a").expect("admission");
    let closing_owner = owner.clone();
    let close = tokio::spawn(async move { closing_owner.close_scope("session-a", 0).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !owner.state.lock().closing.contains("session-a") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("close gate activated");
    assert!(owner.admit("session-a").is_err());
    assert!(!close.is_finished());
    drop(admitted);
    let cursor = tokio::time::timeout(Duration::from_secs(1), close)
        .await
        .expect("close barrier")
        .expect("join close")
        .expect("close accepted");
    assert_eq!(cursor, owner.snapshot("session-a").cursor);
    assert!(owner.admit("session-a").is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_close_cannot_report_success_after_scope_reopens() {
    let owner = ShellTasks::new();
    let admitted = owner.admit("session-a").expect("admission");
    let closing_owner = owner.clone();
    let close = tokio::spawn(async move { closing_owner.close_scope("session-a", 0).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !owner.snapshot("session-a").closing {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("close gate activated");
    drop(admitted);
    let reopened = owner
        .open_scope("session-a", 0)
        .expect("settled scope reopens");
    assert_eq!(reopened.epoch, 1);
    assert!(
        close.await.expect("join close").is_err(),
        "old close is stale"
    );
    assert!(!owner.snapshot("session-a").closing);
}

#[tokio::test]
async fn cancelled_request_cannot_escape_scope_close_barrier() {
    let directory = tempfile::tempdir().unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    let mut owner = ShellTasks::new();
    owner.spawn_gate = Some(Arc::clone(&gate));
    let launch_owner = owner.clone();
    let cwd = directory.path().to_string_lossy().into_owned();
    let caller = tokio::spawn(async move {
        launch_owner
            .spawn_scoped("sleep 30".into(), cwd, None, Some("session-a"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while owner
            .state
            .lock()
            .inflight
            .get("session-a")
            .copied()
            .unwrap_or(0)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owner admitted spawn");
    caller.abort();
    let closing_owner = owner.clone();
    let close = tokio::spawn(async move { closing_owner.close_scope("session-a", 0).await });
    tokio::task::yield_now().await;
    assert!(
        !close.is_finished(),
        "barrier must wait for detached registration"
    );
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(5), close)
        .await
        .expect("barrier")
        .expect("join")
        .expect("epoch");
    let snapshot = owner.snapshot("session-a");
    assert_eq!(
        snapshot.tasks.len(),
        1,
        "cancelled request must leave a discoverable task"
    );
    owner.cancel(&snapshot.tasks[0].task.task.task_id).unwrap();
    tokio::time::timeout(Duration::from_secs(5), owner.shutdown())
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn shell_completion_is_queryable_after_client_like_owner_clone_is_dropped() {
    let dir = tempfile::tempdir().expect("workspace");
    let owner = ShellTasks::new();
    let second_connection = owner.clone();
    let task = owner
        .spawn(
            "printf 'workspace-task-ok'".into(),
            dir.path().to_string_lossy().into_owned(),
            None,
        )
        .await
        .expect("start shell task");
    drop(owner);

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let result = second_connection
                .get(&task.task_id)
                .expect("task remains owned");
            if result.task.status().is_terminal() {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("shell must complete");
    assert_eq!(result.task.status(), TaskStatus::Completed);
    let TaskPayload::Completed { result } = result.task.payload else {
        panic!("terminal task must carry CallToolResult")
    };
    assert_eq!(result.get("isError").and_then(|v| v.as_bool()), Some(false));
    assert!(result.get("structuredContent").is_some());
    assert!(second_connection.get("unknown-task").is_err());
    assert_eq!(
        second_connection.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

#[tokio::test]
async fn cancellation_is_scoped_to_known_task_id() {
    let dir = tempfile::tempdir().expect("workspace");
    let owner = ShellTasks::new();
    let task = owner
        .spawn_scoped(
            "trap '' TERM; printf ready > ready; while :; do sleep 1; done".into(),
            dir.path().to_string_lossy().into_owned(),
            None,
            Some("session-a"),
        )
        .await
        .expect("start shell task");
    tokio::time::timeout(Duration::from_secs(2), async {
        while !dir.path().join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("shell trap ready");
    assert!(owner.cancel("not-this-task").is_err());
    owner.cancel(&task.task_id).expect("cancel task");
    assert_eq!(
        owner.get(&task.task_id).expect("task").task.status(),
        TaskStatus::Working
    );
    assert!(owner.snapshot("session-a").tasks[0]
        .terminal_transition_id
        .is_none());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if owner.get(&task.task_id).expect("task").task.status() == TaskStatus::Cancelled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelled after process cleanup");
    assert!(owner.snapshot("session-a").tasks[0]
        .terminal_transition_id
        .is_some());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), owner.shutdown())
            .await
            .expect("shutdown must await process cleanup"),
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

#[tokio::test]
async fn cancelling_after_completion_preserves_terminal_result() {
    let dir = tempfile::tempdir().expect("workspace");
    let owner = ShellTasks::new();
    let task = owner
        .spawn(
            "printf done".into(),
            dir.path().to_string_lossy().into_owned(),
            None,
        )
        .await
        .expect("start task");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if owner
                .get(&task.task_id)
                .expect("task")
                .task
                .status()
                .is_terminal()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("task deadline");
    owner
        .cancel(&task.task_id)
        .expect("terminal cancel is idempotent");
    assert_eq!(
        owner.get(&task.task_id).expect("task result").task.status(),
        TaskStatus::Completed
    );
    assert_eq!(
        owner.shutdown().await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}
