//! Tracing subscriber 初始化（带日志轮转）

use std::path::{Path, PathBuf};

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

/// TracingGuard 在 Drop 时保证日志被 flush。
///
/// 当前版本 `tracing` 0.1.x 的 `set_global_default` 不返回 guard，
/// 因此 TracingGuard 作为生命周期标记存在，真正的 flush 由
/// `RollingFileAppender` 的 Drop（程序退出时 OS 回收文件句柄）保证。
pub struct TracingGuard;

impl Drop for TracingGuard {
    fn drop(&mut self) {
        // tracing 0.1.x global subscriber 无法被替换/清理；
        // RollingFileAppender 在程序退出时由 OS 关闭文件句柄，
        // 阻塞式同步写入确保日志不丢失。
    }
}

/// 解析日志落盘目标 `(目录, 文件名前缀)`。
///
/// `RUST_LOG_FILE` 为裸文件名（如 `peri.log`）时 `Path::parent()` 返回**空路径**而非
/// `None`，直接当目录使用会让 `RollingFileAppender` 读取目录失败，并把
/// `Error reading the log directory/files` 打进 stderr。目录必须始终保持为可读的
/// 真实路径：空 parent 与无 parent 一律归一为当前目录。
fn resolve_log_target(log_file: Option<&str>, service_name: &str) -> (PathBuf, String) {
    let Some(path) = log_file else {
        let directory = dirs_next::home_dir()
            .map(|home| home.join(".peri").join("logs"))
            .unwrap_or_else(std::env::temp_dir);
        return (directory, service_name.to_string());
    };

    let path = Path::new(path);
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let file_prefix = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "agent-tui".to_string());
    (directory, file_prefix)
}

/// 初始化 tracing，输出到日志文件（TUI 模式下避免干扰界面）。
///
/// 日志使用 `tracing-appender` 按天轮转，保留最近 5 个轮转文件。
pub fn init_tracing(service_name: &str) -> TracingGuard {
    let is_json = std::env::var("RUST_LOG_FORMAT").as_deref() == Ok("json");
    let log_file = std::env::var("RUST_LOG_FILE").ok();

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,peri_middlewares::mcp=warn,peri_middlewares::plugin=warn,rmcp=warn")
    });

    let (log_dir, file_prefix) = resolve_log_target(log_file.as_deref(), service_name);

    std::fs::create_dir_all(&log_dir).unwrap_or_else(|error| {
        panic!("cannot create log directory {}: {error}", log_dir.display())
    });
    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(file_prefix)
        .max_log_files(5)
        .build(&log_dir)
        .unwrap_or_else(|error| {
            panic!(
                "cannot create rolling file appender in {}: {error}",
                log_dir.display()
            )
        });

    if is_json {
        let subscriber = Registry::default()
            .with(filter)
            .with(fmt::layer().json().with_writer(file_appender));
        tracing::subscriber::set_global_default(subscriber)
            .expect("Unable to set global subscriber");
    } else {
        let subscriber = Registry::default()
            .with(filter)
            .with(fmt::layer().with_writer(file_appender).with_ansi(false));
        tracing::subscriber::set_global_default(subscriber)
            .expect("Unable to set global subscriber");
    }

    TracingGuard
}

#[cfg(test)]
#[path = "subscriber_test.rs"]
mod tests;
