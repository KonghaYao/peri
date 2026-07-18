//! acp-hub —— ACP Session 分流器
//!
//! 用法: acp-hub [OPTIONS] [-- <child-command> ...]
//!
//! 默认子进程命令为 `peri acp`。
//! Hub 作为主进程暴露单一 stdio，将不同 session 的请求
//! 路由到独立的 ACP 子进程中执行。

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "acp-hub", about = "ACP Session 分流器")]
struct Cli {
    /// 人类可读日志格式（默认 JSON）
    #[arg(long)]
    pretty: bool,

    /// 日志级别 (trace/debug/info/warn/error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// 子进程启动超时秒数
    #[arg(long, default_value = "10")]
    spawn_timeout: u64,

    /// 子进程请求超时秒数
    #[arg(long, default_value = "300")]
    child_timeout: u64,

    /// 子进程启动命令及参数（-- 之后），默认 `peri acp`
    #[arg(last = true)]
    child_command: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();

    // 默认子进程命令：peri acp
    if cli.child_command.is_empty() {
        cli.child_command = vec!["peri".to_string(), "acp".to_string()];
    }

    // 日志初始化（输出到 stderr）
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("acp_hub={}", cli.log_level)));

    if cli.pretty {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .with_target(true)
            .with_writer(std::io::stderr)
            .init();
    }

    // 就绪信号
    eprintln!(
        "[acp-hub] ready, pid={}, child_cmd=\"{}\"",
        std::process::id(),
        cli.child_command.join(" ")
    );

    // 启动 tokio runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;

    rt.block_on(async {
        acp_hub::hub::run_hub(acp_hub::hub::HubConfig {
            child_cmd: cli.child_command,
            spawn_timeout: cli.spawn_timeout,
            child_timeout: cli.child_timeout,
        })
        .await
    })?;

    Ok(())
}
