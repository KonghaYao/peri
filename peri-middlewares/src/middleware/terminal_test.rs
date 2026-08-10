use std::time::Instant;

#[cfg(unix)]
use peri_agent::agent::async_tasks::TaskManager;
use peri_agent::tools::BaseTool;

use super::*;

#[tokio::test]
async fn test_bash_normal_command() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"command": "echo hello"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("hello"));
}

#[tokio::test]
async fn test_bash_nonzero_exit_code() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"command": "exit 42"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("42"), "应包含退出码: {result}");
}

/// 验证超时后在合理时间内返回，且进程组（bash + 全部子进程）被清理
#[tokio::test]
async fn test_bash_timeout_returns_quickly() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let start = Instant::now();

    // Windows 用 ping 模拟 sleep，Unix 用 sleep
    let (sleep_cmd, timeout_ms) = if cfg!(target_os = "windows") {
        ("ping -n 60 127.0.0.1", 1000)
    } else {
        ("sleep 60", 1000)
    };

    let result = tool
        .invoke(
            serde_json::json!({
                "command": sleep_cmd,
                "timeout": timeout_ms
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    let elapsed = start.elapsed();

    // 应在约 1 秒内返回（不超过 3 秒），不等待 sleep 60 完成
    assert!(
        elapsed.as_secs() < if cfg!(target_os = "windows") { 8 } else { 3 },
        "超时后应快速返回，实际耗时 {:?}",
        elapsed
    );
    assert!(
        err_msg.contains("timed out"),
        "返回值应包含超时提示: {err_msg}"
    );
}

#[tokio::test]
async fn test_bash_stderr_captured() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"command": "echo err >&2"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("err"), "stderr 应被捕获: {result}");
}

#[test]
fn test_truncate_output_line_count_accurate() {
    // 生成不含末尾换行的多行文本，避免 split('\n') 产生额外空行
    let lines: Vec<String> = (0..3000).map(|i| format!("line {}", i)).collect();
    let input = lines.join("\n");
    assert_eq!(input.split('\n').count(), 3000);
    let result = truncate_output(&input);
    assert!(
        result.contains("3000 total lines"),
        "应显示正确的总行数: {result}"
    );
    // 应保留头部和尾部
    assert!(result.contains("line 0"), "应保留第一行: {result}");
    assert!(result.contains("line 2999"), "应保留最后一行: {result}");
    assert!(
        result.contains("lines truncated"),
        "应显示截断信息: {result}"
    );
}

#[test]
fn test_truncate_output_no_truncation_when_small() {
    let result = truncate_output("hello\nworld");
    assert_eq!(result, "hello\nworld");
}

#[test]
fn test_truncate_output_char_limit() {
    let long_line = "x".repeat(200_000);
    let result = truncate_output(&long_line);
    assert!(result.contains("byte limit"), "应截断超长输出: {result}");
}

#[test]
fn test_truncate_output_preserves_tail() {
    // 3000 行，尾部包含关键信息
    let mut lines: Vec<String> = (0..2999).map(|i| format!("line {}", i)).collect();
    lines.push("CRITICAL ERROR: test failed".to_string());
    let input = lines.join("\n");
    let result = truncate_output(&input);
    // 尾部关键行应保留
    assert!(
        result.contains("CRITICAL ERROR"),
        "截断后应保留尾部关键信息: {result}"
    );
    assert!(result.contains("line 0"), "应保留头部: {result}");
}

#[test]
fn test_bash_description_extended() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let desc = tool.description();
    assert!(desc.contains("Usage:"), "description 应包含 Usage 段落");
    assert!(
        desc.contains("dedicated tool"),
        "description 应强调优先使用专用工具"
    );
    assert!(desc.contains("timeout"), "description 应提及超时");
    assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
}

/// timeout=0 表示不超时（前台/后台通用）；这里验证显式正超时下 echo 正常完成。
#[tokio::test]
async fn test_bash_timeout_clamped_to_minimum() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let start = Instant::now();
    // timeout = 2000 → clamp 不生效，echo quick 应正常完成（PowerShell 冷启动较慢）
    let result = tool
        .invoke(
            serde_json::json!({
                "command": "echo quick",
                "timeout": 5000
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();
    assert!(result.contains("quick"), "echo quick 应正常输出: {result}");
    assert!(
        elapsed.as_millis() < 8000,
        "应快速完成，实际耗时 {:?}",
        elapsed
    );
}

/// 显式超时 600000 毫秒应被允许（上限）
#[tokio::test]
async fn test_bash_timeout_maximum_accepted() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "command": "echo ok",
                "timeout": 600000
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("ok"));
}

#[test]
#[allow(non_snake_case)]
fn test_tool_name_is_Bash() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    assert_eq!(tool.name(), "Bash");
}

#[tokio::test]
async fn test_bash_default_timeout_is_15_seconds() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    // 不传 timeout → 默认 15000ms = 15s
    let result = tool
        .invoke(
            serde_json::json!({"command": "echo ok"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("ok"));
}

#[tokio::test]
async fn test_bash_legacy_params_ignored() {
    // description 是 schema 未声明的字段，残留应被静默忽略（不影响执行）
    // 注：run_in_background 现已支持（见 issue bg-tasks-unified-management），不再是 legacy
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "command": "echo ok",
                "description": "test description",
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("ok"));
}

#[test]
fn test_bash_schema_no_legacy_params() {
    // description 从未声明为 BashTool 参数；run_in_background 现已支持（见 bg-tasks-unified-management）
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let params = tool.parameters();
    let props = params["properties"].as_object().unwrap();
    assert!(
        !props.contains_key("description"),
        "schema 不应声明 description 参数"
    );
    assert!(props.contains_key("command"), "command 应保留");
    assert!(props.contains_key("timeout"), "timeout 应保留");
    assert!(
        props.contains_key("run_in_background"),
        "run_in_background 现已支持，schema 应声明"
    );
}

#[test]
fn test_truncate_output_persists_full_content_on_lines_truncation() {
    let lines: Vec<String> = (0..3000).map(|i| format!("line {}", i)).collect();
    let input = lines.join("\n");
    let result = truncate_output(&input);
    assert!(
        result.contains("Read tool"),
        "应包含 Read tool 提示: {result}"
    );
    assert!(
        result.contains("peri-tool-output-"),
        "应包含临时文件路径: {result}"
    );
}

#[test]
fn test_truncate_output_persists_full_content_on_byte_truncation() {
    let long_line = "x".repeat(200_000);
    let result = truncate_output(&long_line);
    assert!(result.contains("Read tool"), "字节截断也应持久化: {result}");
    assert!(
        result.contains("peri-tool-output-"),
        "字节截断应包含文件路径: {result}"
    );
}

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

    // 回调应收到 success=false 且含 "timed out" 的结果
    let notif = rx
        .recv()
        .await
        .expect("bg 超时后应触发 on_bg_complete 回调");
    assert!(!notif.success, "超时结果应为失败");
    assert!(
        notif.output.contains("timed out"),
        "输出应含超时提示: {}",
        notif.output
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
    assert!(
        notif.output.contains("second"),
        "通知应含完整输出: {}",
        notif.output
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
/// 完成回调收到 success=true 含 "done"，active_count 归零。
#[cfg(unix)]
#[tokio::test]
async fn test_sync_timeout_promotes_to_background() {
    let registry = Arc::new(TaskManager::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
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
    assert!(
        notif.output.contains("done"),
        "输出应含 done: {}",
        notif.output
    );
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

// ── stdin null + 超时诊断分流（issue: bash 错误原因定位）──────────────────────

/// stdin 重定向为 /dev/null：read 立即 EOF 返回，不挂死到超时。
/// 旧行为（stdin 继承终端）下该命令会永久阻塞等待输入。
#[cfg(unix)]
#[tokio::test]
async fn test_bash_stdin_null_read_fails_fast() {
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
    let registry = Arc::new(TaskManager::new());
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
    let registry = Arc::new(TaskManager::new());
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
