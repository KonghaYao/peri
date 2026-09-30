use super::*;
use peri_agent::agent::async_tasks::*;
#[cfg(unix)]
use std::time::Duration;

fn make_registry() -> BackgroundTaskRegistry {
    BackgroundTaskRegistry::new()
}

/// cancel() 应杀死整个进程组（bash 为组长）：sh/sleep 子进程不得孤儿存活创建 marker。
/// 命令 `sh -c 'sleep 2; touch marker'`：若只杀 bash 单进程（旧行为），sh 孤儿会在
/// 2s 时 touch；等 3s 断言 marker 不存在可区分新旧行为。
#[cfg(unix)]
#[tokio::test]
async fn test_cancel_kills_process_group() {
    let registry = make_registry();
    let marker =
        std::env::temp_dir().join(format!("peri-cancel-pg-{}.marker", uuid::Uuid::new_v4()));
    let marker_path = marker.to_string_lossy().to_string();

    // spawn 带子进程的命令（sh → sleep），bash 为进程组组长；不设 kill_on_drop
    let mut cmd = shell_command(&format!("sh -c 'sleep 2; touch {}'", marker_path), &[]);
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn().unwrap();
    let pid = child.id().unwrap();

    let task = BackgroundTask {
        id: "bg-shell-cancel".to_string(),
        agent_name: "bg-shell".to_string(),
        prompt_summary: "cancel kills process group".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Shell,
        cancel_handle: BgCancelHandle::Kill(Some(Box::new(move || {
            kill_process_group(pid, "KILL")
        }))),
        cancel_token: None,
        pid: Some(pid),
        output_preview: None,
        agent_inbox: None,
    };
    registry.register_with_kind(task).unwrap();
    assert_eq!(registry.active_count(), 1);

    registry.cancel("bg-shell-cancel").unwrap();
    assert_eq!(registry.active_count(), 0);

    // 等 3s（> sleep 2）：若进程组未被杀，sh/sleep 孤儿会创建 marker
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert!(!marker.exists(), "进程组应被杀死，marker 不应被创建");
    let _ = std::fs::remove_file(&marker);

    // child 句柄 drop（进程已被 cancel 杀死，无孤儿残留）
    drop(child);
}

// ── 进程包装（shell_command）──

#[test]
fn test_shell_command_unix_bash_c() {
    let cmd = shell_command("echo", &["hello"]);
    let formatted = format!("{cmd:?}");
    #[cfg(unix)]
    {
        assert!(
            formatted.contains("bash"),
            "expected bash, got: {formatted}"
        );
        assert!(
            formatted.contains("-c"),
            "expected -c flag, got: {formatted}"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            formatted.contains("powershell"),
            "expected powershell, got: {formatted}"
        );
        assert!(
            formatted.contains("-Command"),
            "expected -Command flag, got: {formatted}"
        );
        assert!(
            formatted.contains("-NoProfile"),
            "expected -NoProfile flag, got: {formatted}"
        );
    }
}

#[test]
fn test_shell_command_no_args() {
    let cmd = shell_command("ls", &[]);
    let formatted = format!("{cmd:?}");
    #[cfg(unix)]
    {
        assert!(
            formatted.contains("bash"),
            "expected bash, got: {formatted}"
        );
        assert!(
            formatted.contains("ls"),
            "expected 'ls' in command, got: {formatted}"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            formatted.contains("powershell"),
            "expected powershell, got: {formatted}"
        );
        assert!(
            formatted.contains("ls"),
            "expected 'ls' in command, got: {formatted}"
        );
    }
}

#[test]
fn test_shell_command_multi_args() {
    let cmd = shell_command("npx", &["-y", "@anthropic/mcp-server"]);
    let formatted = format!("{cmd:?}");
    #[cfg(unix)]
    {
        assert!(
            formatted.contains("bash"),
            "expected bash, got: {formatted}"
        );
        assert!(
            formatted.contains("npx"),
            "expected 'npx', got: {formatted}"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            formatted.contains("powershell"),
            "expected powershell, got: {formatted}"
        );
        assert!(
            formatted.contains("npx"),
            "expected 'npx', got: {formatted}"
        );
        // 多参数应被拼接到命令字符串中
        assert!(
            formatted.contains("@anthropic/mcp-server"),
            "expected @anthropic/mcp-server in command, got: {formatted}"
        );
    }
}

/// 回归测试：Windows 上 `command` 含空格时，不能被 PowerShell 单引号
/// 包围成字符串字面量。否则 `powershell -Command "'ping ...'"` 会把
/// `'ping ...'` 当作字符串 expression 直接 echo 出来，而不是执行命令。
///
/// 触发场景：Bash 工具调用 `shell_command("ping -n 60 127.0.0.1", &[])`，
/// 测试期望 1s 超时返回 Err，实际返回 Ok("ping -n 60 127.0.0.1\r\n")。
#[test]
fn test_shell_command_windows_command_not_string_literal() {
    let cmd = shell_command("ping -n 60 127.0.0.1", &[]);
    let formatted = format!("{cmd:?}");
    #[cfg(windows)]
    {
        // 错误形态：command 被单引号包围（PowerShell 字符串字面量）
        assert!(
            !formatted.contains("'ping -n 60 127.0.0.1'"),
            "command 被错误地用 PowerShell 单引号包围成字符串字面量，会导致 -Command echo 出字符串而非执行命令: {formatted}"
        );
    }
    #[cfg(not(windows))]
    {
        let _ = &formatted;
    }
}

/// 回归测试：Windows 上 args 仍应被 PowerShell 单引号 escape，
/// 防止 `$` `` ` `` `(` `)` `{` `}` `;` `|` `&` `@` `#` 等 metacharacter
/// 被 PowerShell 解析为代码（与 commit b689cc39 的安全意图一致）。
#[test]
fn test_shell_command_windows_args_still_escaped() {
    let cmd = shell_command("echo", &["$HOME", "a;b"]);
    let formatted = format!("{cmd:?}");
    #[cfg(windows)]
    {
        // 含 $ 或 ; 的 args 应被单引号包围成 PowerShell 字面量
        assert!(
            formatted.contains("'$HOME'"),
            "含 $ 的 arg 应被 PowerShell 单引号 escape: {formatted}"
        );
        assert!(
            formatted.contains("'a;b'"),
            "含 ; 的 arg 应被 PowerShell 单引号 escape: {formatted}"
        );
    }
    #[cfg(not(windows))]
    {
        let _ = &formatted;
    }
}

// ── 输出落盘（persist_truncated_output）──

#[test]
fn test_persist_writes_file_and_returns_hint() {
    let content = "line1\nline2\nline3";
    let hint = persist_truncated_output(content);
    // 提示应包含文件名
    assert!(
        hint.contains("peri-tool-output-"),
        "hint should contain filename: {hint}"
    );
    // 提示应引导读取落盘文件——必须用**模型面名字**（裸名已无提供面）。名字逐字
    // 断言注册表的冻结字面量：不用查表派生期望值，否则「查询改坏」会自洽通过。
    assert!(
        hint.contains("use `Read` to view complete content"),
        "hint should guide to the effective read tool: {hint}"
    );
    // 从提示中提取文件路径并验证内容
    let prefix = "saved to ";
    let suffix = " — use `Read`";
    let path_start = hint.find(prefix).unwrap() + prefix.len();
    let path_end = hint[path_start..]
        .find(suffix)
        .map(|i| path_start + i)
        .unwrap_or(hint.len());
    let path = &hint[path_start..path_end];
    let saved = std::fs::read_to_string(path).unwrap();
    assert_eq!(saved, content);
    std::fs::remove_file(path).ok();
}

#[test]
fn test_persist_empty_string() {
    let hint = persist_truncated_output("");
    // 空内容也应生成包含路径的提示，且同样用模型面名字引导读取（逐字断言冻结字面量）
    assert!(
        hint.contains("use `Read` to view complete content"),
        "empty content should also produce the effective-name hint: {hint}"
    );
    // 验证空文件确实被写入，并清理
    let prefix = "saved to ";
    let suffix = " — use `Read`";
    let path_start = hint.find(prefix).unwrap() + prefix.len();
    let path_end = hint[path_start..]
        .find(suffix)
        .map(|i| path_start + i)
        .unwrap_or(hint.len());
    let path = &hint[path_start..path_end];
    let saved = std::fs::read_to_string(path).unwrap();
    assert_eq!(saved, "");
    std::fs::remove_file(path).ok();
}
