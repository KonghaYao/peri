//! 会话选择参数的进程级校验：非法组合必须在启动会话/模型之前以参数错误退出。
//!
//! 只覆盖已确认的最小拒绝集：无值 `-r`、`-p -r`、`-c -r`。其余会话参数保持现状。

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

fn run_peri(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_peri"))
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("PERI_CONFIG_FILE")
        .env_remove("PERI_META_TEST_INTERNAL_ERROR")
        .args(args)
        .output()
        .unwrap()
}

fn assert_argument_error(output: &Output, fragments: &[&str]) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    for fragment in fragments {
        assert!(
            stderr.contains(fragment),
            "missing {fragment:?} in stderr: {stderr}"
        );
    }
}

fn assert_no_session_database(home: &Path) {
    assert!(
        !home.join(".peri/threads/threads.db").exists(),
        "参数校验必须在建立会话库之前失败"
    );
}

#[test]
fn resume_without_value_exits_with_argument_error() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = run_peri(home.path(), cwd.path(), &["-r"]);
    assert_argument_error(&output, &["--resume", "session ID"]);
    assert_no_session_database(home.path());
}

#[test]
fn print_mode_rejects_resume_before_startup() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = run_peri(home.path(), cwd.path(), &["-p", "-r", SESSION_ID]);
    assert_argument_error(&output, &["print", "--resume"]);
    assert_no_session_database(home.path());
}

#[test]
fn continue_and_resume_conflict_exits_with_argument_error() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = run_peri(home.path(), cwd.path(), &["-c", "-r", SESSION_ID]);
    assert_argument_error(&output, &["--continue", "--resume"]);
    assert_no_session_database(home.path());
}
