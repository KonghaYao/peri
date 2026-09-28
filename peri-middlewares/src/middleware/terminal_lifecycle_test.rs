use super::*;

#[cfg(unix)]
#[tokio::test]
async fn test_cancelled_foreground_shell_retains_owner_until_process_exit() {
    let _process_env = crate::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let manager = Arc::new(TaskManager::new());
    let tool = BashTool::new(fixture.path().to_str().unwrap()).with_task_manager(manager.clone());
    let command = tokio::spawn(async move {
        tool.invoke(
            serde_json::json!({"command": "echo $$ > ready; sleep 60", "timeout": 0}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.path().join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut shutdown = peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref());
    // 进程仍活着时必须持有 owner，不能仅因 shutdown 请求就报告完成。
    assert!(futures::poll!(&mut shutdown).is_pending());
    command.abort();
    assert!(command.await.unwrap_err().is_cancelled());
    assert_eq!(
        shutdown.await,
        peri_acp_types::tasks::TaskShutdownReport::Complete,
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_successful_shell_keeps_redirected_background_process_owned() {
    let _process_env = crate::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let manager = Arc::new(TaskManager::new());
    let tool = BashTool::new(fixture.path().to_str().unwrap()).with_task_manager(manager.clone());
    let output = tool.invoke(serde_json::json!({
        "command": "(while [ ! -f release ]; do sleep 0.01; done; printf done > survived) >/dev/null 2>&1 &",
        "timeout": 0
    }), peri_agent::tools::ToolContext::new(&[], ".")).await.unwrap();
    assert!(output.contains("background task"), "{output}");
    assert_eq!(manager.active_count(), 1);
    std::fs::write(fixture.path().join("release"), "go").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.path().join("survived").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background process must remain alive after Bash returns");
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

/// 父 shell 的成功退出不能作为其剩余后台进程的退出码。
#[cfg(unix)]
#[tokio::test]
async fn test_redirected_background_process_reports_unknown_exit_code() {
    let _process_env = crate::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let manager = Arc::new(TaskManager::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let tool = BashTool::new(fixture.path().to_str().unwrap())
        .with_task_manager(manager.clone())
        .with_on_bg_complete(Arc::new(move |result, _| {
            let _ = tx.send(result.clone());
        }));
    let output = tool.invoke(serde_json::json!({
        "command": "(while [ ! -f release ]; do sleep 0.01; done; exit 7) >/dev/null 2>&1 &",
        "timeout": 0
    }), peri_agent::tools::ToolContext::new(&[], ".")).await.unwrap();
    assert!(output.contains("background task"), "{output}");
    std::fs::write(fixture.path().join("release"), "go").unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("descendant completion notification")
        .unwrap();
    let files = result.shell_output.as_ref().expect("output references");
    // 文件已发布给后台任务，worker 结束后仍须可读。
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    for path in [files.stdout_path.as_ref(), files.stderr_path.as_ref()]
        .into_iter()
        .flatten()
    {
        std::fs::read(path).expect("published file remains readable");
        std::fs::remove_file(path).unwrap();
    }
    assert_eq!(files.exit_code, None, "descendant status was never waited");
    assert!(result.to_notification().contains("退出码未知"));
}

#[cfg(unix)]
async fn assert_nohup_pipe_timeout_promotes(redirect: &str) {
    let _process_env = crate::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let manager = Arc::new(TaskManager::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let tool = BashTool::new(fixture.path().to_str().unwrap())
        .with_task_manager(manager.clone())
        .with_on_bg_complete(Arc::new(move |result, _| {
            let _ = tx.send(result.clone());
        }));
    // nohup 的输出不是 TTY，不会自动脱离工具的管道；release 保证子进程活过前台期限。
    let command = format!(
        "nohup sh -c 'while [ ! -f release ]; do sleep 0.01; done; printf late-out; printf late-err >&2; exit 7' {redirect} & printf leader-out; printf leader-err >&2"
    );
    let output = timeout(
        Duration::from_secs(3),
        tool.invoke_output(
            serde_json::json!({"command": command, "timeout": 100}),
            peri_agent::tools::ToolContext::new(&[], "."),
        ),
    )
    .await;
    let active_before_release = manager.active_count();
    // 即使旧实现触发外层超时，也先释放 fixture 并排空 owner，避免失败测试遗留进程。
    std::fs::write(fixture.path().join("release"), "go").unwrap();
    let completed = if active_before_release > 0 {
        Some(timeout(Duration::from_secs(5), rx.recv()).await)
    } else {
        None
    };
    let shutdown = peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await;

    let output = output
        .expect("父 shell 退出后，继承管道的 nohup 子进程不能绕过同步超时")
        .unwrap();
    let evidence = output.execution.expect("超时执行证据");
    assert_eq!(evidence.status, ToolExecutionStatus::RunningAfterTimeout);
    assert_eq!(evidence.exit_code, None);
    assert_eq!(active_before_release, 1);
    assert!(output.text.contains("background task"));
    assert!(output.text.contains("the shell has exited"));
    let completed = completed
        .unwrap()
        .expect("后台输出排空后必须通知完成")
        .unwrap();
    assert_eq!(Some(completed.task_id), evidence.task_id);
    let files = completed.shell_output.unwrap();
    assert!(files.complete);
    assert_eq!(
        files.exit_code, None,
        "父 shell 的 0 不能冒充 nohup 子进程的退出码"
    );
    let stdout = std::fs::read_to_string(files.stdout_path.as_ref().unwrap()).unwrap();
    let stderr = std::fs::read_to_string(files.stderr_path.as_ref().unwrap()).unwrap();
    assert!(stdout.contains("leader-out"));
    assert!(stderr.contains("leader-err") && stderr.contains("late-err"));
    assert_eq!(stdout.contains("late-out"), redirect.is_empty());
    assert_eq!(manager.active_count(), 0);
    assert_eq!(
        shutdown,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    for path in [files.stdout_path, files.stderr_path, evidence.output_ref]
        .into_iter()
        .flatten()
    {
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn test_nohup_inherited_pipes_respect_foreground_timeout() {
    assert_nohup_pipe_timeout_promotes("").await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_nohup_inherited_stderr_respects_foreground_timeout() {
    assert_nohup_pipe_timeout_promotes(">/dev/null").await;
}

#[cfg(unix)]
#[tokio::test]
async fn test_nohup_pipe_timeout_without_manager_stops_descendant() {
    let _process_env = crate::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let tool = BashTool::new(fixture.path().to_str().unwrap());
    let output = timeout(
        Duration::from_secs(3),
        tool.invoke_output(
            serde_json::json!({
                "command": "nohup sh -c 'while [ ! -f release ]; do sleep 0.01; done; printf survived > survived' & printf partial",
                "timeout": 100
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        ),
    ).await;
    std::fs::write(fixture.path().join("release"), "go").unwrap();
    let output = output.expect("无管理器时也不能无限等管道 EOF").unwrap();
    let evidence = output.execution.unwrap();
    assert_eq!(evidence.status, ToolExecutionStatus::TimedOut);
    assert_eq!(evidence.task_id, None);
    assert!(!fixture.path().join("survived").exists());
    let partial = evidence.output_ref.unwrap();
    assert!(std::fs::read_to_string(&partial)
        .unwrap()
        .contains("partial"));
    std::fs::remove_file(partial).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn test_nohup_pipe_timeout_promotion_remains_cancellable() {
    let _process_env = crate::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let manager = Arc::new(TaskManager::new());
    let tool = BashTool::new(fixture.path().to_str().unwrap()).with_task_manager(manager.clone());
    let output = timeout(
        Duration::from_secs(3),
        tool.invoke_output(
            serde_json::json!({"command": "nohup sleep 60 &", "timeout": 100}),
            peri_agent::tools::ToolContext::new(&[], "."),
        ),
    )
    .await;
    let active_before_shutdown = manager.active_count();
    let shutdown = peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await;
    let output = output.expect("必须返回可取消的后台任务").unwrap();
    let evidence = output.execution.unwrap();
    assert_eq!(evidence.status, ToolExecutionStatus::RunningAfterTimeout);
    assert!(evidence.task_id.is_some());
    assert_eq!(active_before_shutdown, 1);
    assert_eq!(
        shutdown,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    assert_eq!(manager.active_count(), 0);
    if let Some(path) = evidence.output_ref {
        std::fs::remove_file(path).unwrap();
    }
}
