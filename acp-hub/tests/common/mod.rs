//! test-child 二进制路径解析等测试辅助函数

use std::io::BufRead;
use std::path::PathBuf;
use std::process::Stdio;

/// 返回 test-child 二进制的路径
pub fn test_child_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_test-child"))
}

/// 返回包含 --crash-after 参数的 test-child 命令
pub fn test_child_crash_after(n: usize) -> Vec<String> {
    vec![
        test_child_path().to_string_lossy().to_string(),
        "--crash-after".to_string(),
        n.to_string(),
    ]
}

/// 启动 Hub 进程，返回 (stdin, stdout_reader, child)
pub fn start_hub(
    child_cmd: &[String],
) -> (
    std::process::ChildStdin,
    std::io::BufReader<std::process::ChildStdout>,
    std::process::Child,
) {
    let hub_path = env!("CARGO_BIN_EXE_acp-hub");

    let mut cmd = std::process::Command::new(hub_path);
    cmd.arg("--spawn-timeout")
        .arg("5")
        .arg("--child-timeout")
        .arg("5")
        .arg("--");
    for arg in child_cmd {
        cmd.arg(arg);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = cmd.spawn().expect("无法启动 Hub");

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let reader = std::io::BufReader::new(stdout);

    (stdin, reader, child)
}

/// 发送 JSON-RPC 请求
pub fn send_req(
    stdin: &mut impl std::io::Write,
    id: i64,
    method: &str,
    params: &serde_json::Value,
) {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let line = serde_json::to_string(&req).unwrap();
    writeln!(stdin, "{}", line).unwrap();
    stdin.flush().unwrap();
}

/// 向 Hub 发送消息并读取下一个有 id 的响应
pub fn send_and_recv(
    _stdin: &mut impl std::io::Write,
    reader: &mut std::io::BufReader<impl std::io::Read>,
) -> serde_json::Value {
    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut line = String::new();
    for _ in 0..20 {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    if v.get("id").is_some() && v.get("id") != Some(&serde_json::Value::Null) {
                        return v;
                    }
                }
            }
            Err(_) => break,
        }
    }
    panic!("未收到有效响应");
}
