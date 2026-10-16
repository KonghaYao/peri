//! Bash 完成路径的执行证据契约：状态与退出码必须描述**整个进程组**，
//! 不能拿前台 shell 的退出码冒充仍在运行的后台子进程，也不能用命令文本
//! （`&&` / `2>&1`）代替进程组的真实观测来降级状态。
use super::*;

/// 有界后台子进程：等待 release 文件，最多自旋 600 次（≈6s）后自行退出。
/// 断言失败时也不会留下无人回收的进程（测试中途 panic 也受此上界保护）。
const LEFTOVER_CHILD: &str = "(n=0; while [ ! -f release ] && [ $n -lt 600 ]; do sleep 0.01; n=$((n+1)); done; printf survived > survived) >/dev/null 2>&1 &";

#[cfg(unix)]
#[tokio::test]
async fn test_logical_and_operator_reports_completed_status() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let tool = BashTool::new(fixture.path().to_str().unwrap());
    let output = timeout(
        Duration::from_secs(10),
        tool.invoke_output(
            serde_json::json!({"command": "echo alpha && echo beta"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        ),
    )
    .await
    .expect("逻辑与命令必须有界返回")
    .unwrap();
    let evidence = output.execution.expect("完成路径必须带执行证据");
    assert_eq!(
        evidence.status,
        ToolExecutionStatus::Completed,
        "`&&` 不产生脱离进程组的子进程，状态应是完成: {}",
        output.text
    );
    assert_eq!(evidence.exit_code, Some(0));
    assert!(output.text.contains("alpha") && output.text.contains("beta"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_stdout_redirect_operator_reports_completed_status() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let tool = BashTool::new(fixture.path().to_str().unwrap());
    let output = timeout(
        Duration::from_secs(10),
        tool.invoke_output(
            serde_json::json!({"command": "echo alpha 2>&1"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        ),
    )
    .await
    .expect("重定向命令必须有界返回")
    .unwrap();
    let evidence = output.execution.expect("完成路径必须带执行证据");
    assert_eq!(
        evidence.status,
        ToolExecutionStatus::Completed,
        "`2>&1` 只是重定向，状态应是完成: {}",
        output.text
    );
    assert_eq!(evidence.exit_code, Some(0));
}

/// 无任务管理器时，活着的未被登记子进程仍如实报 Unknown，且不能声明退出码。
#[cfg(unix)]
#[tokio::test]
async fn test_untracked_leftover_child_reports_unknown_without_exit_code() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let tool = BashTool::new(fixture.path().to_str().unwrap());
    let output = timeout(
        Duration::from_secs(10),
        tool.invoke_output(
            serde_json::json!({"command": LEFTOVER_CHILD, "timeout": 5000}),
            peri_agent::tools::ToolContext::new(&[], "."),
        ),
    )
    .await
    .expect("后台语法命令必须有界返回")
    .unwrap();
    let evidence = output.execution.expect("完成路径必须带执行证据");
    assert_eq!(
        evidence.status,
        ToolExecutionStatus::Unknown,
        "未登记的存活子进程无法核对: {}",
        output.text
    );
    assert_eq!(
        evidence.exit_code, None,
        "前台 shell 的退出码不属于仍在运行的整组"
    );
    assert_eq!(evidence.task_id, None);
    // 子进程确实活着：放行后必须自行结束并留下标记，证明 Unknown 不是误报。
    std::fs::write(fixture.path().join("release"), "go").unwrap();
    timeout(Duration::from_secs(5), async {
        while !fixture.path().join("survived").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("被放行的子进程应自行退出");
}

/// 有任务管理器时，剩余进程登记为后台任务：状态 Running 携带 task_id，
/// 但不得同时声明前台 shell 的退出码（整组尚未结束）。
#[cfg(unix)]
#[tokio::test]
async fn test_tracked_leftover_child_reports_running_without_exit_code() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let manager = Arc::new(peri_mcp_common::create_local_task_manager());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let tool = BashTool::new(fixture.path().to_str().unwrap())
        .with_task_manager(manager.clone())
        .with_on_bg_complete(Arc::new(move |result, _| {
            tx.send(result.clone()).map_err(|error| error.to_string())
        }));
    let output = timeout(
        Duration::from_secs(10),
        tool.invoke_output(
            serde_json::json!({"command": LEFTOVER_CHILD, "timeout": 5000}),
            peri_agent::tools::ToolContext::new(&[], "."),
        ),
    )
    .await
    .expect("后台语法命令必须有界返回")
    .unwrap();
    let evidence = output.execution.expect("完成路径必须带执行证据");
    assert_eq!(
        evidence.status,
        ToolExecutionStatus::Running,
        "剩余进程已登记为后台任务: {}",
        output.text
    );
    assert!(evidence.task_id.is_some(), "登记路径必须回报 task_id");
    assert_eq!(
        evidence.exit_code, None,
        "整组仍在运行时不得声明退出码（离线分析按非终态校验）"
    );
    std::fs::write(fixture.path().join("release"), "go").unwrap();
    let result = timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("后台任务结束应通知")
        .unwrap();
    let files = result.shell_output.as_ref().expect("输出引用");
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
    for path in [
        files.stdout_path.as_ref(),
        files.stderr_path.as_ref(),
        evidence.output_ref.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        let _ = std::fs::remove_file(path);
    }
}
