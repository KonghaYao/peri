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
  peri-browser install kitesurf
  peri-browser start kitesurf [--port N] [--bind IP:PORT]
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

`install kitesurf` 下载并执行官方安装脚本；`start kitesurf` 在引擎就绪后
前台运行网关，Ctrl-C 只停止网关。";

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Bare invocation or the explicit `serve` alias.
    Serve(Vec<String>),
    InstallKitesurf,
    StartKitesurf(StartOptions),
    Help,
    Version,
}

/// Options for `start kitesurf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartOptions {
    pub port: u16,
    pub bind: SocketAddr,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CliError {
    #[error("install requires a target; supported: kitesurf")]
    MissingInstallTarget,
    #[error("unknown install target {0:?}; supported: kitesurf")]
    UnknownInstallTarget(String),
    #[error("start requires a target; supported: kitesurf")]
    MissingStartTarget,
    #[error("unknown start target {0:?}; supported: kitesurf")]
    UnknownStartTarget(String),
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
        Some((command, rest)) if command == "start" => parse_start(rest),
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

fn parse_start(rest: &[String]) -> Result<Command, CliError> {
    let Some((target, flags)) = rest.split_first() else {
        return Err(CliError::MissingStartTarget);
    };
    if target != "kitesurf" {
        return Err(CliError::UnknownStartTarget(target.clone()));
    }
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
                let bind = value
                    .parse::<SocketAddr>()
                    .map_err(|_| CliError::InvalidBind)?;
                if !is_bind_ip(bind.ip()) || bind.port() == 0 {
                    return Err(CliError::InvalidBind);
                }
                options.bind = bind;
            }
            _ => return Err(CliError::UnknownOption(flag.to_owned())),
        }
    }
    Ok(Command::StartKitesurf(options))
}

#[cfg(test)]
#[path = "cli_test.rs"]
mod tests;
