#[cfg(target_os = "windows")]
use super::pty_session::normalize_crlf;
use super::pty_session::PtySession;
use std::io::Read;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// 跨平台获取测试用 shell。
///
/// Windows 上选用 cmd.exe 而非 powershell.exe：PowerShell 5.1 会对 stdout
/// 做「是否重定向」检测，在 ConPTY 场景下输出编码/交互行为不稳定（历史 CI
/// 上 3 个读取测试全部超时）。cmd.exe 的 echo/回显是纯字节流（OEM 编码下
/// ASCII 内容不变形），是验证 ConPTY 读写链路的最稳 shell。
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
/// 超时返回时，通过共享 buffer 带回已读到的部分数据，让断言消息能展示
/// 真实输出内容（区分「完全无输出」与「读到数据但编码/内容不匹配」）。
fn drain_until(reader: Box<dyn Read + Send>, target: &str, timeout: Duration) -> String {
    let target = target.to_string();
    let (tx, rx) = mpsc::channel::<String>();
    // 读线程与主线程共享累积结果：正常结束走 channel，超时路径从共享
    // buffer 取 partial 输出，避免诊断信息在超时时丢失。
    let shared = Arc::new(Mutex::new(String::new()));
    let shared_task = shared.clone();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut accumulated = String::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&chunk[..n]).into_owned();
                    accumulated.push_str(&text);
                    shared_task.lock().unwrap().push_str(&text);
                    if accumulated.contains(&target) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(accumulated);
    });
    match rx.recv_timeout(timeout) {
        Ok(s) => s,
        Err(_) => shared.lock().unwrap().clone(),
    }
}

/// 断言输出包含目标串；失败时附带 hex 预览。
///
/// UTF-16LE 等非 UTF-8 输出经 `from_utf8_lossy` 后字符间会夹 NUL 字节，
/// `contains` 必然失败且字符串预览看不出原因，hex 预览可暴露真实字节。
fn assert_contains(output: &str, target: &str, ctx: &str) {
    assert!(
        output.contains(target),
        "{ctx}，实际: {output:?} (len={})\nhex 前 300 字节: {}",
        output.len(),
        output
            .as_bytes()
            .iter()
            .take(300)
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
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
    // Windows: cmd /C echo hello（cmd 输出为纯字节流，无 PowerShell 编码智能）
    let (shell, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd.exe", vec!["/C", "echo hello"])
    } else {
        ("bash", vec!["-c", "echo hello"])
    };

    let (mut session, reader) =
        PtySession::spawn(shell, &args, 80, 24, None).expect("spawn 应成功");

    let output = drain_until(reader, "hello", Duration::from_secs(10));
    assert_contains(&output, "hello", "输出应包含 hello");

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
    assert_contains(&output, "ping", "回显应包含 ping");

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
        ("cmd.exe", vec!["/C", "cd"])
    } else {
        ("bash", vec!["-c", "pwd"])
    };

    let cwd_str = cwd.to_str().expect("temp_dir 应为有效 UTF-8").to_string();
    let (session, reader) =
        PtySession::spawn(shell, &args, 80, 24, Some(&cwd_str)).expect("spawn 应成功");

    let output = drain_until(reader, last_segment, Duration::from_secs(10));
    assert_contains(
        &output,
        last_segment,
        &format!("输出应包含 cwd 末段 {last_segment}"),
    );

    std::thread::sleep(Duration::from_millis(300));
    drop(session);
}

#[cfg(target_os = "windows")]
#[test]
fn test_normalize_crlf_bare_r_becomes_rn() {
    assert_eq!(normalize_crlf(b"peri\r"), b"peri\r\n");
}

#[cfg(target_os = "windows")]
#[test]
fn test_normalize_crlf_rn_unchanged() {
    assert_eq!(normalize_crlf(b"peri\r\n"), b"peri\r\n");
}

#[cfg(target_os = "windows")]
#[test]
fn test_normalize_crlf_bare_n_becomes_rn() {
    assert_eq!(normalize_crlf(b"peri\n"), b"peri\r\n");
}

#[cfg(target_os = "windows")]
#[test]
fn test_normalize_crlf_multiple_bare_r() {
    assert_eq!(normalize_crlf(b"a\rb\rc\r"), b"a\r\nb\r\nc\r\n");
}

#[cfg(target_os = "windows")]
#[test]
fn test_normalize_crlf_multiple_bare_n() {
    assert_eq!(normalize_crlf(b"a\nb\n"), b"a\r\nb\r\n");
}

#[cfg(target_os = "windows")]
#[test]
fn test_normalize_crlf_mixed_r_and_rn() {
    assert_eq!(normalize_crlf(b"a\rb\r\nc\r"), b"a\r\nb\r\nc\r\n");
}

#[cfg(target_os = "windows")]
#[test]
fn test_normalize_crlf_mixed_n_and_rn() {
    assert_eq!(normalize_crlf(b"a\nb\r\nc\r"), b"a\r\nb\r\nc\r\n");
}

#[cfg(target_os = "windows")]
#[test]
fn test_normalize_crlf_no_cr_or_n_unchanged() {
    assert_eq!(normalize_crlf(b"hello world"), b"hello world");
}

#[cfg(target_os = "windows")]
#[test]
fn test_normalize_crlf_empty() {
    assert!(normalize_crlf(b"").is_empty());
}
