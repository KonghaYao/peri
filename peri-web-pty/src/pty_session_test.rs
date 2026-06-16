use super::pty_session::PtySession;
use std::io::Read;
use std::sync::mpsc;
use std::time::Duration;

/// 跨平台获取测试用 shell。
fn test_shell() -> &'static str {
    if cfg!(target_os = "windows") {
        "cmd.exe"
    } else {
        std::env::var("SHELL")
            .unwrap_or_else(|_| "/bin/bash".to_string())
            .leak()
    }
}

/// 在独立线程中循环读取 reader 并累积输出，主线程通过 channel 超时控制。
///
/// Windows ConPTY 启动时会先发出大量 ANSI escape preamble（清屏、光标控制、
/// 颜色等），单次 read 往往只拿到 preamble 头部，读不到命令实际输出。
/// 循环累积直到包含 `target`、EOF 或超时，才能稳定跨过 preamble。
///
/// 超时后读线程可能仍阻塞在 read 上，由测试进程退出时清理。
fn drain_until(reader: Box<dyn Read + Send>, target: &str, timeout: Duration) -> String {
    let target = target.to_string();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut accumulated = String::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    accumulated.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    if accumulated.contains(&target) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(accumulated);
    });
    rx.recv_timeout(timeout).unwrap_or_default()
}

#[test]
fn test_pty_session_spawn_returns_handles() {
    let (session, _reader) =
        PtySession::spawn(test_shell(), &[], 80, 24, None).expect("spawn 应成功");
    // master/writer/child 字段已就绪，drop 时自动 kill
    drop(session);
}

#[test]
fn test_pty_session_read_receives_echo_output() {
    // Unix: bash -c 'echo hello'，Windows: cmd /c echo hello
    let (shell, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd.exe", vec!["/c", "echo hello"])
    } else {
        ("bash", vec!["-c", "echo hello"])
    };

    let (mut session, reader) =
        PtySession::spawn(shell, &args, 80, 24, None).expect("spawn 应成功");

    let output = drain_until(reader, "hello", Duration::from_secs(10));
    assert!(output.contains("hello"), "输出应包含 hello，实际: {output}");

    // 在 macOS 上 portable-pty 的 try_wait 需要等子进程被 waitpid 回收，
    // reader.read() 返回后进程未必已被回收，留出时间等待 reap
    std::thread::sleep(Duration::from_millis(300));
    let exit = session.try_wait_exit().expect("try_wait 应成功");
    assert!(exit.is_some(), "子进程应已退出");
    drop(session);
}

#[test]
fn test_pty_session_write_feeds_stdin() {
    // 用 cat / cmd 交互式回显
    let (shell, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd.exe", vec![])
    } else {
        ("cat", vec![])
    };

    let (mut session, reader) =
        PtySession::spawn(shell, &args, 80, 24, None).expect("spawn 应成功");

    session.write(b"ping\n").expect("write 应成功");

    let output = drain_until(reader, "ping", Duration::from_secs(10));
    assert!(output.contains("ping"), "回显应包含 ping，实际: {output}");

    session.kill().expect("kill 应成功");
    drop(session);
}

#[test]
fn test_pty_session_resize_does_not_panic() {
    let (mut session, _reader) =
        PtySession::spawn(test_shell(), &[], 80, 24, None).expect("spawn 应成功");
    session.resize(120, 40).expect("resize 应成功");
    drop(session);
}

#[test]
fn test_pty_session_spawn_uses_cwd() {
    // 用系统 temp dir 作为 cwd，避免硬编码 /tmp 在 Windows 上不合法。
    // 断言路径末段（如 Temp / T）以规避 Windows 短名 / 路径分隔符差异。
    let cwd = std::env::temp_dir();
    let last_segment = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .expect("temp_dir 应有 file_name");

    let (shell, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd.exe", vec!["/c", "cd"])
    } else {
        ("bash", vec!["-c", "pwd"])
    };

    let cwd_str = cwd.to_str().expect("temp_dir 应为有效 UTF-8").to_string();
    let (session, reader) =
        PtySession::spawn(shell, &args, 80, 24, Some(&cwd_str)).expect("spawn 应成功");

    let output = drain_until(reader, last_segment, Duration::from_secs(10));
    assert!(
        output.contains(last_segment),
        "输出应包含 cwd 末段 {last_segment}，实际: {output}"
    );

    std::thread::sleep(Duration::from_millis(300));
    drop(session);
}
