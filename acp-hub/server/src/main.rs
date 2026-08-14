//! acp-hub-server 二进制：CLI + 装配（Feature F2，`docs/plans/f2-auth-config.md` §3.3）。
//!
//! ```
//! acp-hub-server [run] [--listen <addr>] [--port <port>] [--config <path>]
//!                [--data-dir <dir>] [--config-dir <dir>] [--log-level <lvl>] [--json-log]
//! acp-hub-server token list
//! acp-hub-server token generate --name <name> [--role instance|full|read-only]
//! acp-hub-server token revoke <token_id>
//! ```
//!
//! `run` 为默认子命令（常驻进程主形态）；`token` 子命令组管理凭据（直写
//! `<config_dir>/tokens.toml`，0600）。

use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::{FromArgMatches, Parser, Subcommand};

use acp_hub_server::auth::audit::audit;
use acp_hub_server::auth::{AuthService, TokenRole, TokenStore, TOKENS_FILE};
use acp_hub_server::config::{self, CliOverrides, Config};
use acp_hub_server::persist::{PersistConfig, Store};

#[derive(Parser)]
#[command(
    name = "acp-hub-server",
    about = "acp-hub 中心控制面（认证、控制面、ACPChannel、聚合器、DocManager）",
    version,
    subcommand_required = false
)]
struct Cli {
    /// 配置文件路径（覆盖默认 ~/.config/acp-hub/config.toml；仅 CLI 提供，无 env）
    #[arg(long)]
    config: Option<PathBuf>,
    /// JSON 格式日志（默认人类可读）
    #[arg(long)]
    json_log: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 启动服务（默认子命令）
    Run(CliOverrides),
    /// token 凭据管理（直写 <config_dir>/tokens.toml）
    #[command(subcommand)]
    Token(TokenArgs),
}

#[derive(Subcommand)]
enum TokenArgs {
    /// 列出全部 token（视图对象，无 token 本体，§9.2.1）
    List,
    /// 生成新 token（完整 token 仅 stdout 打印一次；审计只含 token_id）
    Generate {
        /// token 名称（instance：hostname；client：运维命名）
        #[arg(long)]
        name: String,
        /// 角色：instance|full|read-only（默认 full）
        #[arg(long, default_value = "full")]
        role: TokenRole,
    },
    /// 吊销 token（即刻生效）
    Revoke {
        /// token_id
        token_id: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let Cli {
        config,
        json_log,
        command,
    } = cli;
    match command {
        None => run(config, json_log),
        Some(Command::Run(overrides)) => run_with(config, json_log, overrides),
        Some(Command::Token(args)) => token_command(config, json_log, args),
    }
}

/// `run`（默认）：加载配置 → 初始化日志 → 目录/token bootstrap → 启动横幅。
///
/// clap `env` 注入只在子命令（`Run(CliOverrides)`）解析路径生效；无子命令
/// 时 `Cli::parse` 不会解析 `CliOverrides`，`default()` 即丢失全部
/// `ACP_HUB_*` env 覆盖（E2 根因：目录/端口覆盖静默失效）。此处以空 argv
/// 经 `augment_args` 重新注入 env 后再加载配置。
fn run(config: Option<PathBuf>, json_log: bool) -> anyhow::Result<()> {
    let cmd = <CliOverrides as clap::Args>::augment_args(clap::Command::new("acp-hub-server"));
    let overrides = CliOverrides::from_arg_matches(&cmd.get_matches_from([""]))
        .map_err(|e| anyhow::anyhow!("run 参数解析失败: {e}"))?;
    run_with(config, json_log, overrides)
}

fn run_with(
    config: Option<PathBuf>,
    json_log: bool,
    overrides: CliOverrides,
) -> anyhow::Result<()> {
    let cfg = Config::load(&overrides, config.as_deref())?;
    config::init_tracing(&cfg.log_level, json_log)?;
    cfg.ensure_dirs()?;

    // 启动 bootstrap（§3.3/§4.3.4）：无未吊销 instance token → 自动生成并
    // 打印到 stderr（token 本体只进终端一次，不进日志）。
    let mut token_store = TokenStore::load(&cfg.config_dir.join(TOKENS_FILE))?;
    if let Some(rec) = token_store.ensure_instance_token()? {
        eprintln!(
            "{}",
            bootstrap_token_notice(&cfg.config_dir, &rec.token, std::io::stderr().is_terminal())
        );
        audit(
            "token.generate",
            None,
            Some(&rec.id),
            "ok",
            std::time::Duration::ZERO,
            None,
        );
    }

    let listen = std::net::SocketAddr::new(cfg.listen_addr, cfg.listen_port);
    eprintln!(
        "[acp-hub-server] starting: listening on {listen}, allow_non_loopback={}, data_dir={}",
        cfg.allow_non_loopback,
        cfg.data_dir.display()
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        // 持久化恢复（§8.4.1：outbox 先行 → last_seq 对齐 → Doc 补齐由
        // StoreSink 镜像重建承担；任一不变量失败 → degraded，Registry
        // Restarting 期间呈现）。
        let persist_cfg = PersistConfig::from(&cfg);
        let store = Arc::new(Store::open(&persist_cfg)?);
        let recovery = store.recover().await;
        if recovery.degraded {
            tracing::warn!(
                warnings = recovery.warnings.len(),
                truncated_bytes = recovery.truncated_total_bytes,
                "persist recovery degraded; serving read-only until recovered (§17.2)"
            );
        }
        // AuthService（instance 双向 / client 单向认证）。
        let auth = Arc::new(tokio::sync::Mutex::new(AuthService::new(token_store)));
        // 控制面装配（F5：全部组件实例化与接线）。
        let hub = acp_hub_server::control::Hub::assemble(&cfg, store, auth).await?;
        // §8.4.1 不变量 5：恢复不变量失败 → Degraded（拒绝新 committed
        // 承诺，§17.2；restarting 门禁在首个 instance hello 对账后解除）。
        if recovery.degraded {
            hub.report_restore_degraded().await;
        }
        // 优雅关闭信号（SIGINT/SIGTERM，§8.6）。
        let signal = async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
                let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
                tokio::select! {
                    _ = sigint.recv() => {}
                    _ = sigterm.recv() => {}
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        };
        hub.run_server(&cfg, signal).await
    })
}

fn bootstrap_token_notice(config_dir: &std::path::Path, token: &str, interactive: bool) -> String {
    if interactive {
        format!(
            "[acp-hub-server] 已自动生成 bootstrap instance token（仅本次打印，请妥善保存）:\n{token}"
        )
    } else {
        format!(
            "[acp-hub-server] 已生成 bootstrap instance token；stderr 非交互终端，密钥未写入日志。记录保存在 {}",
            config_dir.join(TOKENS_FILE).display()
        )
    }
}

/// token 子命令：加载配置（定位 config_dir）→ 操作 tokens.toml。
///
/// 目录语义与 `run` 一致：`ACP_HUB_CONFIG_DIR`/`ACP_HUB_DATA_DIR` env 与
/// `--config-dir` 同样生效（clap env 注入）——避免「run 用临时目录、
/// token 子命令写默认目录」的错位（E2 目录一致性根因）。
fn token_command(config: Option<PathBuf>, json_log: bool, args: TokenArgs) -> anyhow::Result<()> {
    // clap `env` feature 需经 Args 解析才注入（`Default` 不会读 env）；
    // 以空 argv（仅 program name）经 augment_args 解析拿到全部 env 覆盖。
    let cmd = <CliOverrides as clap::Args>::augment_args(clap::Command::new("acp-hub-server"));
    let overrides = CliOverrides::from_arg_matches(&cmd.get_matches_from([""]))
        .map_err(|e| anyhow::anyhow!("token 子命令参数解析失败: {e}"))?;
    let cfg = Config::load(&overrides, config.as_deref())?;
    config::init_tracing(&cfg.log_level, json_log)?;
    cfg.ensure_dirs()?;

    let mut store = TokenStore::load(&cfg.config_dir.join(TOKENS_FILE))?;
    match args {
        TokenArgs::List => {
            for info in store.list() {
                println!("{info}");
            }
        }
        TokenArgs::Generate { name, role } => {
            let start = Instant::now();
            let rec = store.generate(role, &name)?;
            // 完整 token 仅 stdout 打印一次（供复制到 instance/TUI 配置）。
            println!("{}", rec.token);
            audit(
                "token.generate",
                None,
                Some(&rec.id),
                "ok",
                start.elapsed(),
                None,
            );
        }
        TokenArgs::Revoke { token_id } => {
            let start = Instant::now();
            match store.revoke(&token_id)? {
                Some(rec) => {
                    println!("revoked {}", rec.id);
                    audit(
                        "token.revoke",
                        None,
                        Some(&rec.id),
                        "ok",
                        start.elapsed(),
                        None,
                    );
                }
                None => {
                    eprintln!("[acp-hub-server] token {token_id} 不存在或已吊销（幂等）");
                    audit(
                        "token.revoke",
                        None,
                        Some(&token_id),
                        "not_found",
                        start.elapsed(),
                        None,
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::bootstrap_token_notice;

    #[test]
    fn redirected_bootstrap_notice_never_contains_the_token() {
        let secret = "bootstrap-secret-must-not-reach-logs";
        let notice = bootstrap_token_notice(std::path::Path::new("/tmp/acp hub"), secret, false);

        assert!(!notice.contains(secret));
        assert!(notice.contains("/tmp/acp hub/tokens.toml"));
        assert!(notice.contains("密钥未写入日志"));
    }

    #[test]
    fn interactive_bootstrap_notice_keeps_the_one_time_recovery_path() {
        let secret = "terminal-only-bootstrap-secret";
        let notice = bootstrap_token_notice(std::path::Path::new("/unused"), secret, true);

        assert!(notice.contains(secret));
        assert!(notice.contains("仅本次打印"));
    }
}
