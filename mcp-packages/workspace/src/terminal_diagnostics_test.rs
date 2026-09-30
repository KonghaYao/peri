use super::*;

// ── stdin null + 超时诊断分流（issue: bash 错误原因定位）──────────────────────

/// stdin 重定向为 /dev/null：read 立即 EOF 返回，不挂死到超时。
/// 旧行为（stdin 继承终端）下该命令会永久阻塞等待输入。
#[cfg(unix)]
#[tokio::test]
async fn test_bash_stdin_null_read_fails_fast() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let start = Instant::now();
    let result = tool
        .invoke(
            serde_json::json!({"command": "read x; echo \"got:${x:-<eof>}\""}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 3,
        "read 应立即 EOF 返回，实际 {:?}",
        elapsed
    );
    assert!(
        result.contains("got:<eof>"),
        "stdin 为 null 时 read 应读到 EOF: {result}"
    );
}

/// 同步超时 promote 且无输出：文案应如实说明"可能永不自行结束"而非承诺完成。
#[cfg(unix)]
#[tokio::test]
async fn test_sync_timeout_promote_no_output_diagnoses_stall() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let tool =
        BashTool::new(std::env::temp_dir().to_str().unwrap()).with_task_manager(registry.clone());

    let err = tool
        .invoke(
            serde_json::json!({
                "command": "sh -c 'sleep 2'", // 无输出
                "timeout": 200,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("timed out"), "Err 应含 timed out: {err}");
    assert!(
        err.contains("no output produced"),
        "无输出分支应明确说明: {err}"
    );
    assert!(
        err.contains("may never complete on its own"),
        "应说明可能永不自行结束: {err}"
    );
    assert!(
        err.contains("waiting for input"),
        "应提示等待输入的可能原因: {err}"
    );
    assert!(
        err.contains("Process state:"),
        "应附进程状态快照用于定位: {err}"
    );
    assert!(
        err.contains("run_in_background"),
        "应提示服务/守护进程应用后台模式: {err}"
    );
    assert!(err.contains("kill"), "应说明 kill 方式: {err}");

    // 等 promote 续跑任务收尾，避免残留注册
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert_eq!(registry.active_count(), 0, "完成后 active_count 应归零");
}

/// 同步超时 promote 且有输出：文案应如实说明"有进展、续跑合理"。
#[cfg(unix)]
#[tokio::test]
async fn test_sync_timeout_promote_with_output_notes_progress() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(peri_mcp_common::create_local_task_manager());
    let tool =
        BashTool::new(std::env::temp_dir().to_str().unwrap()).with_task_manager(registry.clone());

    let err = tool
        .invoke(
            serde_json::json!({
                "command": "sh -c 'echo progressing; sleep 2'",
                "timeout": 200,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("timed out"), "Err 应含 timed out: {err}");
    assert!(
        err.contains("producing output"),
        "有输出分支应说明有进展: {err}"
    );
    assert!(
        !err.contains("no output produced"),
        "有输出分支不应走无输出文案: {err}"
    );

    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert_eq!(registry.active_count(), 0, "完成后 active_count 应归零");
}

#[test]
fn test_background_cleanup_hints_match_host_platform() {
    for platform in ["linux", "macos", "windows"] {
        let hint = background_cleanup_hint(5036, platform);
        assert!(hint.contains("verify process exit"));
        assert!(hint.contains("completion notification"));
        if platform == "windows" {
            assert!(hint.contains("taskkill /PID 5036 /T"));
            assert!(hint.contains("add `/F`"));
            assert!(hint.contains("parent PID has already exited"));
            assert!(!hint.contains("pgid:"));
            assert!(!hint.contains("kill -TERM"));
        } else {
            assert!(hint.contains("pgid: 5036"));
            assert!(hint.contains("kill -TERM -- -5036"));
            assert!(hint.contains("kill -KILL -- -5036"));
            assert!(!hint.contains("taskkill"));
        }
        assert!(!hint.contains("`kill 5036`"));
    }
}

/// [回归测试] 父 shell 已退、后代持管道时，既有 Bash 整组停止可使原执行链结算。
#[cfg(unix)]
#[tokio::test]
async fn test_bash_group_cleanup_settles_descendant_after_parent_exit() {
    let fixture = tempfile::tempdir().unwrap();
    let manager = Arc::new(peri_mcp_common::create_local_task_manager());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let tool = BashTool::new(fixture.path().to_str().unwrap())
        .with_task_manager(manager.clone())
        .with_on_bg_complete(Arc::new(move |result, _| {
            let _ = tx.send(result.clone());
        }));
    let started = tool
        .invoke(
            serde_json::json!({"command": "sleep 60 & echo $! > descendant.pid", "run_in_background": true}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let pgid = started
        .lines()
        .find_map(|line| line.strip_prefix("pgid: "))
        .unwrap();
    let parent_exited = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let alive = tokio::process::Command::new("kill")
                .args(["-0", pgid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .unwrap()
                .success();
            if fixture.path().join("descendant.pid").exists() && !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let active_before_stop = manager.active_count();
    let stopped = tool
        .invoke(
            serde_json::json!({"command": format!("kill -TERM -- -{pgid}")}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let completed = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;
    let shutdown = peri_acp_types::tasks::TaskManager::shutdown(manager.as_ref()).await;
    parent_exited.expect("fixture parent must exit before cleanup");
    assert_eq!(
        active_before_stop, 1,
        "descendant still owns the output pipes"
    );
    stopped.expect("group termination through Bash must succeed");
    let result = completed
        .expect("existing worker must settle after group exit")
        .unwrap();
    assert!(
        result.shell_output.unwrap().complete,
        "both output pipes must settle"
    );
    assert_eq!(manager.active_count(), 0);
    assert_eq!(
        shutdown,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

// ── command not found 诊断（issue: bash 错误原因定位）────────────────────────

/// 命令不存在（exit 127）：输出尾部附带 PATH 候选或环境类兜底诊断，
/// 供模型直接采取行动（换命令名 / 检查 PATH 与虚拟环境）。
#[tokio::test]
async fn test_bash_command_not_found_appends_hint() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"command": "xx_q1w2e3_not_a_real_cmd_xx"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("[Exit code: 127]"), "{result}");
    assert!(result.contains("not found in PATH"), "{result}");
    assert!(
        result.contains("xx_q1w2e3_not_a_real_cmd_xx"),
        "诊断应点名缺失的命令: {result}"
    );
}

/// 非 127 的非零退出码（命令存在但失败）不追加 command-not-found 诊断。
#[tokio::test]
async fn test_bash_non_127_failure_has_no_command_hint() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"command": "ls /definitely-not-a-real-dir-xx"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(!result.contains("not found in PATH"), "{result}");
}
