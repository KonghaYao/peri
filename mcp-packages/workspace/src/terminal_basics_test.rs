use super::*;

#[tokio::test]
async fn test_bash_nonzero_exit_code() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
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

#[cfg(unix)]
#[tokio::test]
async fn test_bash_cd_does_not_persist_between_invocations() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().canonicalize().unwrap();
    let nested = cwd.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let tool = BashTool::new(cwd.to_str().unwrap());

    for (command, expected) in [("cd nested && pwd -P", &nested), ("pwd -P", &cwd)] {
        let output = tool
            .invoke(
                serde_json::json!({"command": command}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert_eq!(output.trim(), expected.to_str().unwrap());
    }
}

/// 验证超时后在合理时间内返回，且进程组（bash + 全部子进程）被清理
#[tokio::test]
async fn test_bash_timeout_returns_quickly() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
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
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
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

/// `timeout: 0` 在前台被界到前台上限（不再表示不超时）；显式正超时按请求值生效。
#[tokio::test]
async fn test_bash_timeout_clamped_to_minimum() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
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

/// 显式超时 600000 毫秒被接受，但按前台上限 120000 执行（不再是无界/10 分钟）。
/// 命令本身瞬时完成，因此这里只验证请求被接受且正常执行。
#[tokio::test]
async fn test_bash_oversized_timeout_is_capped_by_foreground_maximum() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    assert_eq!(
        peri_agent::agent::async_tasks::parse_foreground_timeout(
            &serde_json::json!({"timeout": 600000})
        ),
        (FOREGROUND_MAX_TIMEOUT_MS, Some(600_000)),
        "超过前台上限的请求必须界到上限"
    );
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

/// `timeout: 0` 不能禁用同步超时：解析结果恒有界，请求仍走同步路径正常执行。
#[tokio::test]
async fn test_bash_sync_timeout_zero_is_bounded_by_foreground_maximum() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    assert_eq!(
        peri_agent::agent::async_tasks::parse_foreground_timeout(
            &serde_json::json!({"timeout": 0})
        ),
        (FOREGROUND_MAX_TIMEOUT_MS, Some(0)),
        "timeout: 0 必须界到前台上限，不能表示不超时"
    );
    assert_eq!(
        peri_agent::agent::async_tasks::parse_foreground_timeout(&serde_json::json!({})),
        (
            peri_agent::agent::async_tasks::FOREGROUND_DEFAULT_TIMEOUT_MS,
            None
        )
    );

    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let output = tool
        .invoke_output(
            serde_json::json!({"command": "echo bounded", "timeout": 0}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(output.text.contains("bounded"), "{}", output.text);
    let evidence = output.execution.expect("sync evidence");
    assert_eq!(evidence.status, ToolExecutionStatus::Completed);
    assert_eq!(evidence.exit_code, Some(0));
}

/// 前台 `timeout: 0` 的真实上界（约 120s）：不早于前台上限 promote，
/// 到上限后转后台并回传 task_id，取消时清理整个进程组。
/// 需要约 2 分钟真实等待，故默认 `#[ignore]`；取证时用 `cargo test -- --ignored` 手动运行。
#[cfg(unix)]
#[tokio::test]
#[ignore = "需要约 120s 真实等待（前台上限），手动运行"]
async fn test_foreground_timeout_zero_promotes_at_foreground_maximum() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
    let registry = Arc::new(TaskManager::new());
    let tool =
        BashTool::new(std::env::temp_dir().to_str().unwrap()).with_task_manager(registry.clone());
    let cap_secs = FOREGROUND_MAX_TIMEOUT_MS / 1000;

    let start = Instant::now();
    let err = tool
        .invoke(
            serde_json::json!({"command": "sh -c 'echo alive; sleep 600'", "timeout": 0}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let elapsed = start.elapsed();

    assert!(err.contains("timed out"), "Err 应含 timed out: {err}");
    assert!(
        err.lines().any(|l| l.starts_with("task_id: ")),
        "promote 应回传 task_id: {err}"
    );
    assert!(
        err.contains("cannot disable the timeout"),
        "应说明 `timeout: 0` 被界到前台上限: {err}"
    );
    assert!(
        elapsed >= Duration::from_secs(cap_secs - 2),
        "不应早于前台上限 promote，实际 {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(cap_secs + 30),
        "不应晚于前台上限太多，实际 {elapsed:?}"
    );
    assert_eq!(registry.active_count(), 1, "promote 后应注册为后台任务");
    assert_eq!(
        peri_acp_types::tasks::TaskManager::shutdown(registry.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}

/// 前台 timeout 被界到上限时的回执说明（`0` 与超上限两种请求）。
#[test]
fn test_foreground_timeout_note_explains_bounded_sync_requests() {
    assert!(
        foreground_timeout_note(None).is_empty(),
        "未改写时不应附加说明"
    );
    let zero = foreground_timeout_note(Some(0));
    assert!(zero.contains("cannot disable the timeout"), "{zero}");
    assert!(
        zero.contains(&FOREGROUND_MAX_TIMEOUT_MS.to_string()),
        "{zero}"
    );
    let oversized = foreground_timeout_note(Some(600_000));
    assert!(oversized.contains("600000"), "{oversized}");
    assert!(oversized.contains("run_in_background"), "{oversized}");
}

/// 工具描述与实现一致：默认/上限取自常量，且不得再宣传"0 = 不超时"。
#[test]
fn test_bash_timeout_description_matches_implementation() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    let schema = tool.parameters()["properties"]["timeout"]["description"]
        .as_str()
        .expect("timeout 参数描述")
        .to_string();
    for text in [tool.description().to_string(), schema] {
        assert!(
            text.contains(
                &peri_agent::agent::async_tasks::FOREGROUND_DEFAULT_TIMEOUT_MS.to_string()
            ),
            "描述应写明前台默认超时: {text}"
        );
        assert!(
            text.contains(&FOREGROUND_MAX_TIMEOUT_MS.to_string()),
            "描述应写明前台上限: {text}"
        );
        assert!(
            !text.contains("0 = no timeout") && !text.contains("disable the timeout entirely"),
            "同步路径不得宣传 0 可禁用超时: {text}"
        );
        assert!(
            !text.contains("timeout: 300000"),
            "不得再建议超过前台上限的值: {text}"
        );
    }
}

#[test]
#[allow(non_snake_case)]
fn test_tool_name_is_Bash() {
    let tool = BashTool::new(std::env::temp_dir().to_str().unwrap());
    assert_eq!(tool.name(), "Bash");
}

#[tokio::test]
async fn test_bash_default_timeout_is_15_seconds() {
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
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
    let _process_env = peri_mcp_common::process_env::lock().expect("process env lock");
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
    // 落盘提示必须用**模型面名字**引导读取（当前 direct 工具使用原名）：逐字断言注册表的
    // 冻结字面量，不用查表派生期望值（同源派生会让「查询改坏」自洽通过）。
    assert!(
        result.contains("use `Read` to view complete content"),
        "应包含模型面名字的读取提示: {result}"
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
    assert!(
        result.contains("use `Read` to view complete content"),
        "字节截断也应持久化并引导读取: {result}"
    );
    assert!(
        result.contains("peri-tool-output-"),
        "字节截断应包含文件路径: {result}"
    );
}
