//! Hand-written CLI dispatch for the single `peri-browser` binary.
//!
//! The first argument selects a subcommand. Anything that is not a known
//! subcommand is passed to [`crate::config::Config::parse_from`], so bare
//! `peri-browser [OPTIONS]` keeps its pre-subcommand behaviour.

use std::net::SocketAddr;

use crate::config::{is_bind_ip, DEFAULT_BIND};
use crate::onboarding::RECOMMENDED_PORT;

/// Help text; also the only place that lists the command surface.
pub const USAGE: &str = "\
peri-browser - Kitesurf MCP and monitor gateway

USAGE:
  peri-browser [OPTIONS]
  peri-browser serve [OPTIONS]
  peri-browser start [OPTIONS]
  peri-browser start kitesurf [--port N] [--bind IP:PORT]
  peri-browser stop [--bind IP:PORT]
  peri-browser install kitesurf
  peri-browser update [kitesurf]
  peri-browser -V|--version
  peri-browser -h|--help

SERVE OPTIONS:
  --bind IP:PORT                 默认 127.0.0.1:7391（PERI_BROWSER_MONITOR_BIND）
  --sandbox-bind IP:PORT         顶层 App 视图与 sandbox 页的监听，默认 127.0.0.1:7392；
                                 off 关闭（PERI_BROWSER_SANDBOX_BIND）
  --app-html PATH                /apps/view 与 /apps/sandbox 返回的真实 MCP App HTML
                                 （≤5 MiB，自带脚本与样式）；缺省用占位页
                                 （PERI_BROWSER_APP_HTML）
  --view-token VALUE             view session API 的 bearer token；缺省时该 API
                                 关闭（PERI_BROWSER_VIEW_TOKEN，推荐环境变量）
  --cdp-url URL                  Kitesurf 建议地址（KITESURF_CDP_URL）；不自动连接
  --secret VALUE                 monitor token 派生（CLI 参数优先于 PERI_BROWSER_GATEWAY_SECRET）
  --idle-yield-ms MS             默认 15000（PERI_BROWSER_IDLE_YIELD_MS）
  --yield-timeout-ms MS          默认 60000（PERI_BROWSER_YIELD_TIMEOUT_MS）
  --request-human-timeout-ms MS  默认 300000（PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS）

START OPTIONS:
  --port N                       引擎端口，默认 18971
  --bind IP:PORT                 网关监听地址，默认 127.0.0.1:7391

`serve` 前台阻塞运行网关；`start` 用同一套网关参数在后台运行，健康检查
通过后返回。`start kitesurf` 先确保引擎就绪，再后台启动网关；拉起的引擎
默认带 `--allow-private-network`，设 `PERI_BROWSER_ALLOW_PRIVATE_NETWORK=0`
可关闭。
状态：$HOME/.peri/browser/run/gateway-<port>.json
日志：$HOME/.peri/browser/logs/gateway-<port>.log
`stop` 只停止 `start` 拉起的网关；`install kitesurf` 下载并执行官方安装脚本。
`update` 重新运行 browser 安装脚本：更新网关并确保 kitesurf 已安装，已安装的
引擎不会升级；`update kitesurf` 无条件重跑 kitesurf 官方安装脚本，安装最新
release。安装器不会停止正在运行的进程：网关或引擎需 `stop` 后再 `start` 才会
用上新版本。";

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Bare invocation or the explicit `serve` alias; foreground.
    Serve(Vec<String>),
    /// `start [OPTIONS]`: background gateway with the serve options.
    StartGateway(Vec<String>),
    InstallKitesurf,
    /// `update`: re-run the browser installer, which keeps kitesurf installed
    /// but does not upgrade it.
    Update,
    /// `update kitesurf`: force the kitesurf installer to fetch the latest
    /// release.
    UpdateKitesurf,
    StartKitesurf(StartOptions),
    Stop(StopOptions),
    Help,
    Version,
}

/// Options for `start kitesurf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartOptions {
    pub port: u16,
    pub bind: SocketAddr,
}

/// Options for `stop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopOptions {
    pub bind: SocketAddr,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CliError {
    #[error("install requires a target; supported: kitesurf")]
    MissingInstallTarget,
    #[error("unknown install target {0:?}; supported: kitesurf")]
    UnknownInstallTarget(String),
    #[error("unknown start target {0:?}; supported: kitesurf or gateway options")]
    UnknownStartTarget(String),
    #[error("update accepts at most one target; unexpected {0:?}; see --help")]
    UnexpectedUpdateArgument(String),
    #[error(
        "unknown update target {0:?}; supported: kitesurf; run `peri-browser update` without a target to update the gateway"
    )]
    UnknownUpdateTarget(String),
    #[error("--port requires a value")]
    MissingPort,
    #[error("--port must be an integer from 1 to 65535")]
    InvalidPort,
    #[error("--bind requires a value")]
    MissingBind,
    #[error("--bind must use 127.0.0.1 or [::1] and a non-zero port")]
    InvalidBind,
    #[error("unknown option {0:?}; see --help")]
    UnknownOption(String),
}

/// Dispatches on the first argument; unmatched input stays on the serve path.
pub fn parse(args: &[String]) -> Result<Command, CliError> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(Command::Help);
    }
    match args.split_first() {
        Some((flag, _)) if flag == "--version" || flag == "-V" => Ok(Command::Version),
        Some((command, rest)) if command == "serve" => Ok(Command::Serve(rest.to_vec())),
        Some((command, rest)) if command == "install" => parse_install(rest),
        Some((command, rest)) if command == "update" => parse_update(rest),
        Some((command, rest)) if command == "start" => parse_start(rest),
        Some((command, rest)) if command == "stop" => parse_stop(rest),
        _ => Ok(Command::Serve(args.to_vec())),
    }
}

fn parse_install(rest: &[String]) -> Result<Command, CliError> {
    let Some((target, extra)) = rest.split_first() else {
        return Err(CliError::MissingInstallTarget);
    };
    if target != "kitesurf" {
        return Err(CliError::UnknownInstallTarget(target.clone()));
    }
    if let Some(flag) = extra.first() {
        return Err(CliError::UnknownOption(flag.clone()));
    }
    Ok(Command::InstallKitesurf)
}

/// `update [kitesurf]`: no target updates the gateway and only ensures the
/// engine is installed; `kitesurf` forces the engine installer to run again.
fn parse_update(rest: &[String]) -> Result<Command, CliError> {
    match rest.split_first() {
        None => Ok(Command::Update),
        Some((target, extra)) if target == "kitesurf" => match extra.first() {
            Some(flag) => Err(CliError::UnexpectedUpdateArgument(flag.clone())),
            None => Ok(Command::UpdateKitesurf),
        },
        Some((target, _)) => Err(CliError::UnknownUpdateTarget(target.clone())),
    }
}

fn parse_start(rest: &[String]) -> Result<Command, CliError> {
    match rest.split_first() {
        None => Ok(Command::StartGateway(Vec::new())),
        Some((target, flags)) if target == "kitesurf" => parse_start_kitesurf(flags),
        Some((flag, _)) if flag.starts_with('-') => Ok(Command::StartGateway(rest.to_vec())),
        Some((target, _)) => Err(CliError::UnknownStartTarget(target.clone())),
    }
}

fn parse_start_kitesurf(flags: &[String]) -> Result<Command, CliError> {
    let mut options = StartOptions {
        port: RECOMMENDED_PORT,
        bind: DEFAULT_BIND
            .parse::<SocketAddr>()
            .map_err(|_| CliError::InvalidBind)?,
    };
    let mut flags = flags.iter();
    while let Some(arg) = flags.next() {
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(key, value)| (key, Some(value)));
        match flag {
            "--port" => {
                let value = inline
                    .map(str::to_owned)
                    .or_else(|| flags.next().cloned())
                    .ok_or(CliError::MissingPort)?;
                options.port = value
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or(CliError::InvalidPort)?;
            }
            "--bind" => {
                let value = inline
                    .map(str::to_owned)
                    .or_else(|| flags.next().cloned())
                    .ok_or(CliError::MissingBind)?;
                let bind = parse_loopback_bind(&value)?;
                if bind.port() == 0 {
                    return Err(CliError::InvalidBind);
                }
                options.bind = bind;
            }
            _ => return Err(CliError::UnknownOption(flag.to_owned())),
        }
    }
    Ok(Command::StartKitesurf(options))
}

fn parse_stop(rest: &[String]) -> Result<Command, CliError> {
    let mut bind = DEFAULT_BIND
        .parse::<SocketAddr>()
        .map_err(|_| CliError::InvalidBind)?;
    let mut flags = rest.iter();
    while let Some(arg) = flags.next() {
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(key, value)| (key, Some(value)));
        match flag {
            "--bind" => {
                let value = inline
                    .map(str::to_owned)
                    .or_else(|| flags.next().cloned())
                    .ok_or(CliError::MissingBind)?;
                bind = parse_loopback_bind(&value)?;
                if bind.port() == 0 {
                    return Err(CliError::InvalidBind);
                }
            }
            _ => return Err(CliError::UnknownOption(flag.to_owned())),
        }
    }
    Ok(Command::Stop(StopOptions { bind }))
}

fn parse_loopback_bind(value: &str) -> Result<SocketAddr, CliError> {
    let bind = value
        .parse::<SocketAddr>()
        .map_err(|_| CliError::InvalidBind)?;
    if !is_bind_ip(bind.ip()) {
        return Err(CliError::InvalidBind);
    }
    Ok(bind)
}

#[cfg(test)]
#[path = "cli_test.rs"]
mod tests;
