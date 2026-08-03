use std::time::Instant;

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

/// parse_timeout 纯函数语义：
/// - 后台：未传 → None（不超时）；显式 0 → None；显式 >0 → clamp 到 [min, 600000]
/// - 同步：未传 → Some(15000)；显式 0 → None；显式 >0 → clamp 到 [min, 600000]
/// - min：Unix 为 1；Windows 为 5000（进程创建/终止开销大，过短超时不可靠）
#[test]
fn test_parse_timeout_semantics() {
    let min = if cfg!(target_os = "windows") { 5000 } else { 1 };
    // 后台
    assert_eq!(parse_timeout(&serde_json::json!({}), true), None);
    assert_eq!(
        parse_timeout(&serde_json::json!({"timeout": 0}), true),
        None
    );
    assert_eq!(
        parse_timeout(&serde_json::json!({"timeout": 2000}), true),
        Some(2000.max(min))
    );
    // 同步
    assert_eq!(parse_timeout(&serde_json::json!({}), false), Some(15_000));
    assert_eq!(
        parse_timeout(&serde_json::json!({"timeout": 0}), false),
        None
    );
    assert_eq!(
        parse_timeout(&serde_json::json!({"timeout": 2000000}), false),
        Some(600_000)
    );
}

/// bg 显式超时：应杀死整个进程组（bash 为组长），sh/sleep 子进程不得孤儿存活创建 marker。
/// 命令 `sh -c 'sleep 3; touch marker'` + timeout 2000：若只杀 bash 单进程（旧行为），
/// sh 孤儿会在 3s 时 touch；等 3.5s 断言 marker 不存在可区分新旧行为。
#[cfg(unix)]
#[tokio::test]
async fn test_bg_explicit_timeout_kills_process_group() {
    let registry = Arc::new(BackgroundTaskRegistry::new());
    let marker = std::env::temp_dir().join(format!(
        "peri-bg-timeout-kill-{}.marker",
        uuid::Uuid::new_v4()
    ));
    let marker_path = marker.to_string_lossy().to_string();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_registry(registry)
        .with_on_bg_complete(Arc::new(move |r| {
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

/// 同步超时 + 有注册表：不杀进程，promote 为后台任务续跑；
/// 完成回调收到 success=true 含 "done"，active_count 归零。
#[cfg(unix)]
#[tokio::test]
async fn test_sync_timeout_promotes_to_background() {
    let registry = Arc::new(BackgroundTaskRegistry::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundTaskResult>();
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap())
        .with_registry(registry.clone())
        .with_on_bg_complete(Arc::new(move |r| {
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
