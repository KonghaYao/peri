//! Tracing subscriber 初始化（带日志轮转）

use std::path::Path;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

pub struct TracingGuard;

impl Drop for TracingGuard {
    fn drop(&mut self) {
        // 无需特殊清理逻辑
    }
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

    let (log_dir, file_prefix) = match &log_file {
        Some(path) => {
            let p = Path::new(path);
            let dir = p
                .parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| Path::new(".").to_path_buf());
            let file_name = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "agent-tui".to_string());
            (dir, file_name)
        }
        None => (std::env::temp_dir(), service_name.to_string()),
    };

    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(file_prefix)
        .max_log_files(5)
        .build(&log_dir)
        .expect("cannot create rolling file appender");

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
