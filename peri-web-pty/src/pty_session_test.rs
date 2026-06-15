use super::*;

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

    let (mut session, mut reader) =
        PtySession::spawn(shell, &args, 80, 24, None).expect("spawn 应成功");

    // 等子进程输出后读
    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut buf = [0u8; 256];
    let n = reader.read(&mut buf).expect("read 应成功");
    assert!(n > 0, "应读到一些字节");
    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(output.contains("hello"), "输出应包含 hello，实际: {output}");

    // 在 macOS 上 portable-pty 的 try_wait 需要等子进程被 waitpid 回收，
    // reader.read() 返回后进程未必已被回收，留出时间等待 reap
    std::thread::sleep(std::time::Duration::from_millis(300));
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

    let (mut session, mut reader) =
        PtySession::spawn(shell, &args, 80, 24, None).expect("spawn 应成功");

    session.write(b"ping\n").expect("write 应成功");

    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut buf = [0u8; 1024];
    let n = reader.read(&mut buf).expect("read 应成功");
    let output = String::from_utf8_lossy(&buf[..n]);
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
    let (shell, args): (&str, Vec<&str>) = if cfg!(target_os = "windows") {
        ("cmd.exe", vec!["/c", "cd"])
    } else {
        ("bash", vec!["-c", "pwd"])
    };

    let (session, mut reader) =
        PtySession::spawn(shell, &args, 80, 24, Some("/tmp")).expect("spawn 应成功");

    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut buf = [0u8; 256];
    let n = reader.read(&mut buf).expect("read 应成功");
    let output = String::from_utf8_lossy(&buf[..n]);
    assert!(output.contains("/tmp"), "输出应包含 /tmp，实际: {output}");

    std::thread::sleep(std::time::Duration::from_millis(300));
    drop(session);
}
