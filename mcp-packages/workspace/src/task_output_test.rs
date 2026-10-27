use super::*;

#[test]
fn utf8_budget_and_terminal_fallback_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let stdout = dir.path().join("stdout");
    let stderr = dir.path().join("stderr");
    std::fs::write(&stdout, "你好abc").unwrap();
    std::fs::write(&stderr, "err").unwrap();
    let logs = TaskOutputLogs {
        stdout_log: Some(stdout.to_string_lossy().into()),
        stderr_log: Some(stderr.to_string_lossy().into()),
    };
    let OutputPage {
        stdout: out,
        stderr: err,
        cursor: next,
        has_more: more,
        ..
    } = read_output(logs.clone(), (0, 0), 4, None).unwrap();
    assert_eq!(
        (out.as_str(), err.as_str(), next.as_str(), more),
        ("你", "e", "3:1", true)
    );
    assert!(read_output(logs.clone(), (1, 0), 32, None).is_err());
    assert!(read_output(logs.clone(), (99, 0), 32, None).is_err());
    let OutputPage {
        stdout: out,
        stderr: err,
        has_more: more,
        ..
    } = read_output(logs, (3, 1), 32, None).unwrap();
    assert_eq!((out.as_str(), err.as_str(), more), ("好abc", "rr", false));
    let OutputPage {
        stdout: out,
        cursor: next,
        has_more: more,
        ..
    } = read_output(TaskOutputLogs::default(), (0, 0), 3, Some("你好".into())).unwrap();
    assert_eq!(
        (out.as_str(), next.as_str(), more),
        ("你", format!("3:{FALLBACK_DONE}").as_str(), true)
    );
    let OutputPage {
        cursor: next,
        has_more: more,
        ..
    } = read_output(
        TaskOutputLogs::default(),
        (3, FALLBACK_DONE),
        3,
        Some("你好".into()),
    )
    .unwrap();
    assert_eq!(next, format!("{FALLBACK_DONE}:{FALLBACK_DONE}"));
    assert!(!more);
    let OutputPage {
        stdout: out,
        has_more: more,
        ..
    } = read_output(
        TaskOutputLogs::default(),
        (FALLBACK_DONE, FALLBACK_DONE),
        3,
        Some("你好".into()),
    )
    .unwrap();
    assert!(out.is_empty() && !more);
}

#[tokio::test]
async fn query_validation_owner_and_no_progress() {
    let tasks = ShellTasks::new();
    let task = tasks.track_promoted_owned("shell-test", Some("A"));
    for args in [
        json!({"task_id":task.task_id,"cursor":"oops"}),
        json!({"task_id":task.task_id,"cursor":"-1:0"}),
        json!({"task_id":task.task_id,"max_bytes":0}),
        json!({"task_id":task.task_id,"max_bytes":32769}),
        json!({"task_id":task.task_id,"wait_ms":-1}),
    ] {
        assert!(query(&tasks, args, Some("A")).await.is_err());
    }
    assert!(query(&tasks, json!({"task_id":task.task_id}), Some("B"))
        .await
        .is_err());
    for index in 1..=3 {
        let content = query(&tasks, json!({"task_id":task.task_id}), Some("A"))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(content["no_progress"], index == 3);
        assert_eq!(content["consecutive_queries"], index);
        assert!(!content.to_string().contains("shell-test"));
    }
}

#[tokio::test(start_paused = true)]
async fn wait_is_bounded_and_does_not_cancel() {
    let tasks = ShellTasks::new();
    let task = tasks.track_promoted_owned("shell-wait", Some("A"));
    let start = tokio::time::Instant::now();
    let content = query(
        &tasks,
        json!({"task_id":task.task_id,"wait_ms":10001}),
        Some("A"),
    )
    .await
    .unwrap()
    .structured_content
    .unwrap();
    assert_eq!(
        start.elapsed(),
        std::time::Duration::from_millis(MAX_WAIT_MS)
    );
    assert_eq!(content["status"], "running");
    assert_eq!(content["wait_timed_out"], true);
    assert_eq!(content["wait_ms"], 10000);
    assert!(content["note"].as_str().unwrap().contains("10000"));
    assert!(!tasks.get("shell-wait").unwrap().task.status().is_terminal());
}

#[tokio::test]
async fn terminal_facts_and_missing_log_fallback_remain_incremental() {
    use peri_acp_types::{event::BackgroundTaskResult, tasks::BgTaskKind};
    let tasks = ShellTasks::new();
    tasks.track_promoted_owned("shell-failed", Some("A"));
    let result: BackgroundTaskResult = serde_json::from_value(json!({
        "task_id":"shell-failed", "agent_name":"bg-shell", "prompt_summary":"fixture",
        "success":false, "output":"你好failure", "tool_calls_count":0, "duration_ms":123, "timed_out":true
    })).unwrap();
    tasks.completion_callback_for(Some("A".into()))(&result, BgTaskKind::Shell).unwrap();
    let first = query(
        &tasks,
        json!({"task_id":"shell-failed","max_bytes":3}),
        Some("A"),
    )
    .await
    .unwrap()
    .structured_content
    .unwrap();
    assert_eq!(first["status"], "failed");
    assert_eq!(first["terminal"], true);
    assert_eq!(first["success"], false);
    assert_eq!(first["timed_out"], true);
    assert_eq!(first["duration_ms"], 123);
    assert_eq!(first["output"]["stdout"], "你");
    let second = query(
        &tasks,
        json!({"task_id":"shell-failed","cursor":first["cursor"]}),
        Some("A"),
    )
    .await
    .unwrap()
    .structured_content
    .unwrap();
    assert_eq!(second["output"]["stdout"], "好failure");
    let third = query(
        &tasks,
        json!({"task_id":"shell-failed","cursor":second["cursor"]}),
        Some("A"),
    )
    .await
    .unwrap()
    .structured_content
    .unwrap();
    assert_eq!(third["output"]["stdout"], "");
    assert_eq!(third["has_more"], false);
    assert_eq!(third["no_progress"], true);
}

#[tokio::test]
async fn unrelated_and_lagged_updates_do_not_lose_terminal_state() {
    use peri_acp_types::{event::BackgroundTaskResult, tasks::BgTaskKind};
    let tasks = ShellTasks::new();
    tasks.track_promoted_owned("shell-race", Some("A"));
    let query_tasks = tasks.clone();
    let waiter = tokio::spawn(async move {
        query(
            &query_tasks,
            json!({"task_id":"shell-race","wait_ms":1000}),
            Some("A"),
        )
        .await
        .unwrap()
    });
    tokio::task::yield_now().await;
    for i in 0..100 {
        tasks.track_promoted_owned(&format!("other-{i}"), Some("B"));
    }
    let result: BackgroundTaskResult = serde_json::from_value(json!({
        "task_id":"shell-race", "agent_name":"bg-shell", "prompt_summary":"fixture",
        "success":true, "output":"finished", "tool_calls_count":0, "duration_ms":1
    }))
    .unwrap();
    tasks.completion_callback_for(Some("A".into()))(&result, BgTaskKind::Shell).unwrap();
    let content = waiter.await.unwrap().structured_content.unwrap();
    assert_eq!(content["status"], "completed");
    assert_eq!(content["wait_timed_out"], false);
    assert_eq!(content["output"]["stdout"], "finished");
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_background_output_and_query_do_not_redeliver_completion() {
    let tasks = ShellTasks::new();
    let mut updates = tasks.subscribe();
    let dir = tempfile::tempdir().unwrap();
    let task = tasks
        .spawn_owned(
            "sleep 0.05; printf hello; printf error >&2".into(),
            dir.path().to_string_lossy().into(),
            None,
            Some("A"),
        )
        .await
        .unwrap();
    let content = query(
        &tasks,
        json!({"task_id":task.task_id,"wait_ms":1000,"max_bytes":3}),
        Some("A"),
    )
    .await
    .unwrap()
    .structured_content
    .unwrap();
    assert_eq!(content["status"], "completed");
    assert_eq!(content["output"]["stdout"], "hel");
    assert_eq!(content["has_more"], true);
    let mut terminals = 0;
    while let Ok(detail) = updates.try_recv() {
        if detail.task.status.is_terminal() {
            terminals += 1;
        }
    }
    let next = query(
        &tasks,
        json!({"task_id":task.task_id,"cursor":content["cursor"]}),
        Some("A"),
    )
    .await
    .unwrap()
    .structured_content
    .unwrap();
    assert_eq!(next["output"]["stdout"], "lo");
    assert_eq!(next["output"]["stderr"], "error");
    assert_eq!(next["has_more"], false);
    assert_eq!(terminals, 1);
    assert!(updates.try_recv().is_err());
    tasks.shutdown().await;
}
