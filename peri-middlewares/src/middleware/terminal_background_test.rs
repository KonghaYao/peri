use super::*;

// ── 后台任务超时语义（issue 2026-08-02-background-task-15s-timeout-kills-and-misreports）──
// parse_timeout / bg_shell_task_id 纯函数测试已随实现迁至
// `peri-agent/src/agent/async_tasks_test.rs`（L1 迁移点），此处不再重复。

/// bg 显式超时：应杀死整个进程组（bash 为组长），sh/sleep 子进程不得孤儿存活创建 marker。
/// 命令 `sh -c 'sleep 3; touch marker'` + timeout 2000：若只杀 bash 单进程（旧行为），
/// sh 孤儿会在 3s 时 touch；等 3.5s 断言 marker 不存在可区分新旧行为。
#[cfg(unix)]
#[tokio::test]
async fn test_bg_explicit_timeout_kills_process_group() {
    let registry = Arc::new(TaskManager::new());
    let marker = std::env::temp_dir().join(format!(
        "peri-bg-timeout-kill-{}.marker",
        uuid::Uuid::new_v4()
    ));
    let marker_path = marker.to_string_lossy().to_string();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let _process_env = crate::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_task_manager(registry)
        .with_on_bg_complete(Arc::new(move |r, _kind| {
            let _ = tx.send(r.clone());
        }));

    let result = tool
        .invoke(
            serde_json::json!({
                "command": format!("sh -c 'sleep 3; touch {}'", marker_path),
                "run_in_background": true,
                "timeout": 2000,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("shell-"), "应返回 task_id: {result}");

    // 回调保留失败与真实超时终态，通知不再携带输出正文。
    let notif = rx
        .recv()
        .await
        .expect("bg 超时后应触发 on_bg_complete 回调");
    assert!(!notif.success, "超时结果应为失败");
    assert!(
        notif.to_notification().contains("超时被终止"),
        "通知必须区分终止和仍在后台运行"
    );
    assert!(notif.timed_out, "超时结果应标记 timed_out");

    // 等 3.5s（> sleep 3）：若进程组未被杀，sh/sleep 孤儿会创建 marker
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert!(!marker.exists(), "子进程不应存活，marker 不应被创建");
    let _ = std::fs::remove_file(&marker);
}

/// run_in_background 任务应在**启动时**注册（BgTaskStarted 立即推送），
/// 运行期间 registry 可见（TUI 展示栏依赖此事件在运行期间显示任务）；
/// 完成后 registry 归零。
///
/// 回归：此前 bg shell 只在完成时 register_with_kind（Started 与 Completed
/// 同时发出），任务运行期间 TUI 的 status 下方展示栏没有条目。
#[cfg(unix)]
#[tokio::test]
async fn test_bg_shell_registered_while_running() {
    let registry = Arc::new(TaskManager::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let _process_env = crate::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_task_manager(registry.clone())
        .with_on_bg_complete(Arc::new(move |r, _kind| {
            let _ = tx.send(r.clone());
        }));

    let result = tool
        .invoke(
            serde_json::json!({
                "command": "sleep 1.2",
                "run_in_background": true,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.lines().any(|l| l.starts_with("pid: ")),
        "应返回 pid 行: {result}"
    );
    assert!(result.contains("kill"), "应说明 kill 方式: {result}");
    let task_id = result
        .lines()
        .find(|l| l.starts_with("task_id: "))
        .expect("应返回 task_id 行")
        .trim_start_matches("task_id: ")
        .to_string();

    // 运行期间（sleep 1.2 尚未结束）：任务必须已注册且可查询
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(registry.active_count(), 1, "运行期间任务应已注册");
    let tasks = registry.list_tasks();
    assert_eq!(tasks.len(), 1, "运行期间应可列出任务");
    assert_eq!(tasks[0].0, task_id, "注册的任务 id 应与返回的 task_id 一致");

    // 完成后：回调收到成功结果，registry 清空
    let notif = rx
        .recv()
        .await
        .expect("bg 完成后应触发 on_bg_complete 回调");
    assert!(notif.success, "sleep 1.2 应成功退出");
    assert_eq!(notif.task_id, task_id);
    assert_eq!(registry.active_count(), 0, "完成后任务应已清理");
}

/// bg shell 的 stdout/stderr 应 tee 到日志文件：返回消息含日志路径，
/// 运行期间 agent 可经 Read 读取部分输出，完成后文件包含全部输出。
#[cfg(unix)]
#[tokio::test]
async fn test_bg_shell_log_file_tee() {
    let registry = Arc::new(TaskManager::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let _process_env = crate::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_task_manager(registry.clone())
        .with_on_bg_complete(Arc::new(move |r, _kind| {
            let _ = tx.send(r.clone());
        }));

    let result = tool
        .invoke(
            serde_json::json!({
                "command": "printf 'first\\n'; sleep 1.5; printf 'second\\n'",
                "run_in_background": true,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // 运行期的实时日志读取指引必须用**模型面名字**（当前 direct 工具使用原名）：逐字断言注册表的
    // 冻结字面量，不用查表派生期望值（同源派生会让「查询改坏」自洽通过）。
    assert!(
        result.contains("— it appends while the command runs (use `Read` to view)"),
        "后台启动回执应指引模型面名字读取实时日志: {result}"
    );
    let log_line = result
        .lines()
        .find(|l| l.contains("stdout.log"))
        .expect("应返回 stdout 日志路径: {result}");
    let log_path = log_line
        .split(' ')
        .find(|t| t.contains("peri-bg-"))
        .expect("日志路径应含 peri-bg- 前缀: {log_line}")
        .to_string();
    let stderr_path = log_line
        .split(' ')
        .find(|t| t.contains("peri-bg-") && t.contains("stderr.log"))
        .expect("应返回 stderr 日志路径: {log_line}")
        .to_string();

    // 运行期间（sleep 1.5 未结束）：日志文件应已含 first，不含 second
    tokio::time::sleep(Duration::from_millis(500)).await;
    let partial = std::fs::read_to_string(&log_path).expect("运行期间应可读日志文件");
    assert!(
        partial.contains("first"),
        "运行期间应已写入 first: {partial}"
    );
    assert!(!partial.contains("second"), "second 尚未输出: {partial}");

    // 完成后：通知到达时日志文件应含全部输出
    let notif = rx
        .recv()
        .await
        .expect("bg 完成后应触发 on_bg_complete 回调");
    assert!(notif.success);
    let files = notif
        .shell_output
        .as_ref()
        .expect("typed output references");
    assert!(files.complete);
    assert_eq!(files.stdout_path.as_deref(), Some(log_path.as_str()));
    assert_eq!(files.exit_code, Some(0));
    assert!(!notif.to_notification().contains("second"));
    // 完成通知的读取指引逐字断言完整短语，避免只匹配工具名。
    assert!(
        notif
            .to_notification()
            .contains("请使用 `Read` 工具按需读取"),
        "完成通知必须用模型面名字引导读取: {}",
        notif.to_notification()
    );
    let full = std::fs::read_to_string(&log_path).expect("完成后应可读日志文件");
    assert!(
        full.contains("first") && full.contains("second"),
        "完成后应含全部输出: {full}"
    );

    let _ = std::fs::remove_file(&log_path);
    let _ = std::fs::remove_file(&stderr_path);
}

/// 同步超时 + 有注册表：不杀进程，promote 为后台任务续跑；
/// 完成回调收到 success=true 和完整输出文件引用，active_count 归零。
#[cfg(unix)]
#[tokio::test]
async fn test_sync_timeout_promotes_to_background() {
    let registry = Arc::new(TaskManager::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let _process_env = crate::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_task_manager(registry.clone())
        .with_on_bg_complete(Arc::new(move |r, _kind| {
            let _ = tx.send(r.clone());
        }));

    let err = tool
        .invoke(
            serde_json::json!({
                "command": "sh -c 'sleep 2; echo done'",
                "timeout": 200,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("timed out"), "Err 应含 timed out: {err}");
    assert!(err.contains("shell-"), "Err 应含 task_id: {err}");
    assert!(
        err.contains("background task"),
        "Err 应说明已转后台续跑: {err}"
    );
    assert!(
        err.lines().any(|l| l.starts_with("pid: ")),
        "Err 应含 pid 行: {err}"
    );
    assert!(err.contains("kill"), "Err 应说明 kill 方式: {err}");
    assert!(
        !err.contains("Note:"),
        "未改写的请求不应附加界上限说明: {err}"
    );
    // 超时前已开始落盘的实时日志路径必须回传（promote 后继续追写）
    let log_line = err
        .lines()
        .find(|l| l.contains("peri-foreground-shell-"))
        .expect("Err 应含实时日志路径行");
    let stdout_log = log_line
        .split("Read the log file ")
        .nth(1)
        .expect("日志行应含 'Read the log file'")
        .split(' ')
        .next()
        .expect("日志路径后应有空格")
        .to_string();
    assert!(
        std::path::Path::new(&stdout_log).exists(),
        "Err 回传的日志文件应已创建: {stdout_log}"
    );

    // Err 中的 task_id 应与回调结果一致
    let task_id = err
        .lines()
        .find(|l| l.starts_with("task_id: "))
        .expect("Err 应含 task_id 行")
        .trim_start_matches("task_id: ")
        .to_string();

    // 约 2s 后续跑任务完成，回调收到成功结果
    let notif = rx
        .recv()
        .await
        .expect("promote 完成后应触发 on_bg_complete 回调");
    assert_eq!(notif.task_id, task_id, "回调任务 id 应与 promote 返回一致");
    assert!(notif.success, "续跑完成应成功");
    let files = notif
        .shell_output
        .as_ref()
        .expect("promoted output references");
    assert!(files.complete);
    assert_eq!(files.exit_code, Some(0));
    assert!(std::fs::read_to_string(files.stdout_path.as_ref().unwrap())
        .unwrap()
        .contains("done"));
    assert!(
        std::fs::read_to_string(&stdout_log)
            .unwrap()
            .contains("done"),
        "promote 续跑的输出应写入回传的同一日志文件: {stdout_log}"
    );
    assert!(!notif.to_notification().contains("done"));
    assert!(!notif.timed_out, "正常完成不应标记 timed_out");

    // complete() 清理后 active_count 归零
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(registry.active_count(), 0, "完成后 active_count 应归零");
}

/// 同步超时 + 无注册表：杀进程组，部分输出落盘；
/// Err 含 "timed out" 与部分输出文件路径，文件内容含已产生输出。
#[cfg(unix)]
#[tokio::test]
async fn test_sync_timeout_without_registry_kills_and_persists_partial() {
    let _process_env = crate::process_env::lock().expect("process env lock");
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap()); // 无 registry
    let err = tool
        .invoke(
            serde_json::json!({
                "command": "sh -c 'echo partial-before-timeout; sleep 30'",
                "timeout": 500,
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("timed out"), "Err 应含 timed out: {err}");
    assert!(
        err.contains("Partial output"),
        "Err 应含 partial output 提示: {err}"
    );
    // 指引必须用**模型面名字**引导读取（当前 direct 工具使用原名）：逐字断言注册表的冻结字面量，
    // 不用查表派生期望值（同源派生会让「查询改坏」自洽通过）。
    assert!(
        err.contains("use `Read` to view captured output so far"),
        "部分输出提示必须用模型面名字引导读取: {err}"
    );

    // 提取落盘文件路径并验证内容
    let hint = err
        .lines()
        .find(|l| l.contains("peri-tool-output-"))
        .expect("Err 应包含部分输出文件路径");
    let path_str = hint
        .split("saved to ")
        .nth(1)
        .expect("提示应含 'saved to'")
        .split(' ')
        .next()
        .expect("路径后应有空格")
        .trim_end_matches(']');
    let file_content = std::fs::read_to_string(path_str).expect("部分输出文件应可读");
    assert!(
        file_content.contains("partial-before-timeout"),
        "部分输出文件应含已产生输出: {file_content}"
    );
    let _ = std::fs::remove_file(path_str);
}
