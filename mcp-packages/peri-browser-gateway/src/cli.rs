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
  peri-browser update
  peri-browser -V|--version
  peri-browser -h|--help

SERVE OPTIONS:
  --bind IP:PORT                 默认 127.0.0.1:7391（PERI_BROWSER_MONITOR_BIND）
  --cdp-url URL                  Kitesurf 建议地址（KITESURF_CDP_URL）；不自动连接
  --secret VALUE                 monitor token 派生（CLI 参数优先于 PERI_BROWSER_GATEWAY_SECRET）
  --idle-yield-ms MS             默认 15000（PERI_BROWSER_IDLE_YIELD_MS）
  --yield-timeout-ms MS          默认 60000（PERI_BROWSER_YIELD_TIMEOUT_MS）
  --request-human-timeout-ms MS  默认 300000（PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS）

START OPTIONS:
  --port N                       引擎端口，默认 18971
  --bind IP:PORT                 网关监听地址，默认 127.0.0.1:7391

`serve` 前台阻塞运行网关；`start` 用同一套网关参数在后台运行，健康检查
通过后返回。`start kitesurf` 先确保引擎就绪，再后台启动网关。
状态：$HOME/.peri/browser/run/gateway-<port>.json
日志：$HOME/.peri/browser/logs/gateway-<port>.log
`stop` 只停止 `start` 拉起的网关；`install kitesurf` 下载并执行官方安装脚本。
`update` 重新运行官方安装脚本（默认同时处理 kitesurf，继承当前环境变量），
正在运行的网关需 `stop` 后重新 `start` 才会用上新版本。";

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Bare invocation or the explicit `serve` alias; foreground.
    Serve(Vec<String>),
    /// `start [OPTIONS]`: background gateway with the serve options.
    StartGateway(Vec<String>),
    InstallKitesurf,
    /// `update`: re-run the official installer, which handles kitesurf too.
    Update,
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
    #[error("update takes no arguments; unexpected {0:?}; see --help")]
    UnexpectedUpdateArgument(String),
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

/// `update` forwards nothing: the installer reads its own environment.
fn parse_update(rest: &[String]) -> Result<Command, CliError> {
    match rest.first() {
        Some(extra) => Err(CliError::UnexpectedUpdateArgument(extra.clone())),
        None => Ok(Command::Update),
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
