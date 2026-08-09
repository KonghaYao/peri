//! acp-instance —— instance daemon（F6，docs/architecture.md §3.2/§12）
//!
//! 每台机器一个 daemon：outbound ws 连 server（`/instance`），收 spawn/kill
//! 指令、管理 ACP 进程树、透明转发 + 断线缓冲（§4.5/§8.5）。
//!
//! 配置优先级：CLI > 环境变量 > 默认值（§9）。不引入配置文件（Cargo.toml 无
//! toml 依赖，M1 instance 侧最小面【决策】，f6-instance.md §9）。

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::EnvFilter;

/// 默认 server 地址（§9【决策】：路径 `/instance` 便于 server 侧路由）。
const DEFAULT_SERVER_URL: &str = "ws://127.0.0.1:8456/instance";

/// 默认数据目录：`~/.local/share/acp-hub/instance/`（§10/§16 语义）。
fn default_data_dir() -> PathBuf {
    dirs_next::home_dir()
        .map(|h| h.join(".local").join("share").join("acp-hub").join("instance"))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[derive(Parser)]
#[command(
    name = "acp-instance",
    about = "acp-hub instance daemon：outbound 连 server、管理 ACP 进程树、透明转发 + 断线缓冲"
)]
struct Cli {
    /// server ws 地址（如 ws://127.0.0.1:8456/instance）
    #[arg(long, env = "ACP_HUB_SERVER_URL")]
    server_url: Option<String>,

    /// instance token 文件路径（必填；44 字符 base64，0600）
    #[arg(long, env = "ACP_HUB_TOKEN_FILE")]
    token_file: Option<PathBuf>,

    /// 数据目录（水位/缓冲）
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// 日志级别 (trace/debug/info/warn/error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// JSON 格式日志（默认人类可读）
    #[arg(long)]
    json_log: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // 日志初始化（输出到 stderr，target 统一 acp_hub::instance）。
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("acp_hub::instance={}", cli.log_level)));
    if cli.json_log {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .with_target(true)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .with_writer(std::io::stderr)
            .init();
    }

    // 配置（CLI > env（clap env 特性）> 默认）。
    let server_url = cli
        .server_url
        .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
    let token_file = cli
        .token_file
        .context("缺少 --token-file（或环境变量 ACP_HUB_TOKEN_FILE）")?;
    let token = std::fs::read_to_string(&token_file)
        .with_context(|| format!("读取 token 文件失败: {}", token_file.display()))?
        .trim()
        .to_string();
    let data_dir = cli.data_dir.unwrap_or_else(default_data_dir);

    let config = acp_instance::hub::InstanceConfig::new(server_url, token, data_dir);

    tracing::info!(target: "acp_hub::instance", data_dir = %config.data_dir.display(),
        "acp-instance 启动");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    rt.block_on(acp_instance::hub::run(config))?;
    Ok(())
}
