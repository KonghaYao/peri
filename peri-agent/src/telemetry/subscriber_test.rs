//! `RUST_LOG_FILE` 落盘目标解析的回归测试。
//!
//! 回归 bug-peri-3.16.5-llm-api-error-abort §6.3：采集日志首行恒为
//! `Error reading the log directory/files: No such file or directory (os error 2)`
//! （来自 `tracing-appender` 读取日志目录失败），裸文件名会让日志目录解析成空路径。

use std::path::PathBuf;

use super::resolve_log_target;

/// 裸文件名（无目录部分）必须落到当前目录：`Path::new("peri.log").parent()` 是
/// `Some("")` 而不是 `None`，空路径会让 appender 读目录失败。
#[test]
fn bare_log_file_name_resolves_to_current_directory() {
    let (directory, prefix) = resolve_log_target(Some("peri.log"), "peri-print");
    assert_eq!(directory, PathBuf::from("."));
    // 前缀取 file_stem（轮转时拼日期后缀，与既有行为一致）。
    assert_eq!(prefix, "peri");
}

/// 无论输入形态如何，解析出的目录都必须非空——空目录是本次回归的根因。
#[test]
fn every_log_file_input_resolves_to_a_non_empty_directory() {
    for input in [
        "peri.log",
        "",
        ".",
        "logs/",
        ".tmp/agent-tui.log",
        "/var/tmp/peri.log",
        "./peri.log",
    ] {
        let (directory, prefix) = resolve_log_target(Some(input), "peri-print");
        assert!(
            !directory.as_os_str().is_empty(),
            "RUST_LOG_FILE={input:?} 解析出空日志目录"
        );
        assert!(
            !prefix.is_empty(),
            "RUST_LOG_FILE={input:?} 解析出空文件名前缀"
        );
    }
}

#[test]
fn nested_relative_log_file_keeps_its_directory_and_stem() {
    let (directory, prefix) = resolve_log_target(Some(".tmp/agent-tui.log"), "agent-tui");
    assert_eq!(directory, PathBuf::from(".tmp"));
    assert_eq!(prefix, "agent-tui");
}

#[test]
fn absolute_log_file_keeps_its_directory_and_stem() {
    let (directory, prefix) = resolve_log_target(Some("/var/tmp/peri.log"), "peri-print");
    assert_eq!(directory, PathBuf::from("/var/tmp"));
    assert_eq!(prefix, "peri");
}

/// 未设置 `RUST_LOG_FILE` 时使用 `~/.peri/logs`（无 home 时回退临时目录）
/// 与 service 名（生产默认路径）。
#[test]
fn default_log_target_is_the_peri_home_logs_directory() {
    let (directory, prefix) = resolve_log_target(None, "agent-tui");
    assert!(directory.is_absolute(), "默认日志目录应为绝对路径");
    match dirs_next::home_dir() {
        Some(home) => assert_eq!(
            directory,
            home.join(".peri").join("logs"),
            "有 home 时应落在 ~/.peri/logs"
        ),
        None => assert_eq!(directory, std::env::temp_dir(), "无 home 时应回退临时目录"),
    }
    assert_eq!(prefix, "agent-tui");
}
