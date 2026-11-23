//! Environment and command-line configuration for the `peri-browser` MCP server.
//!
//! There is deliberately no endpoint configuration: the browser endpoint is
//! chosen at runtime by `browser_connect`. The only install-related input is
//! `KITESURF_INSTALL_DIR`, which locates an existing kitesurf install for
//! `browser_status`; it never influences the connection (README §10).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

/// Command-line surface. Everything else is environment-driven so the host can
/// inject paths without rebuilding the MCP configuration.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "peri-browser",
    version,
    about = "MCP server exposing Kitesurf CDP pages as browser tools",
    after_help = "Environment:\n  KITESURF_INSTALL_DIR              kitesurf install root reported by browser_status (default: $HOME/.kitesurf)\n  PERI_BROWSER_OWNER                page controller owner (default peri-browser)\n  PERI_BROWSER_SESSION_NAME         monitor session label (default: owner)\n  PERI_BROWSER_GATEWAY_SOCK         monitor gateway UDS (default: $TMPDIR/peri-browser-gateway.sock)\n  PERI_BROWSER_PROVISION_LOCK       create/close lock file (default: $TMPDIR/kitesurf-provision.lock)\n  PERI_BROWSER_CONNECT_TIMEOUT_MS   page and browser connection timeout (default 5000)\n  PERI_BROWSER_PAGE_WAIT_MS         page load wait bound (default 30000)\n  PERI_BROWSER_IDLE_YIELD_MS        idle hand-off delay (default 15000)\n  PERI_BROWSER_RECLAIM_WAIT_MS      reclaim wait (default 2000)\n  PERI_BROWSER_YIELD_TIMEOUT_MS     manual takeover wait (default 60000)\n  PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS  hand-off timeout (default 300000)\n  PERI_BROWSER_PROVISION_LOCK_WAIT_MS    lock wait bound (default 10000)\n  PERI_BROWSER_LOCK_STALE_MS        stale lock age (default 30000)\n  PERI_BROWSER_HTTP_ADDR            same as --http when no flag is passed"
)]
pub struct Cli {
    /// Serve MCP Streamable HTTP at ADDR instead of stdio (for example 127.0.0.1:8832)
    #[arg(long, value_name = "ADDR")]
    pub http: Option<SocketAddr>,
}

/// Why a configured value could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("{key} must be a non-negative integer, got {value:?}")]
    InvalidNumber { key: &'static str, value: String },
    #[error("{key} must not be empty")]
    EmptyValue { key: &'static str },
    #[error("invalid HTTP listen address: {0}")]
    InvalidHttpAddr(String),
    #[error("HOME is not set; set KITESURF_INSTALL_DIR to name the kitesurf install root")]
    MissingHome,
}

/// Effective browser MCP configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserConfig {
    pub owner: String,
    pub session_name: String,
    /// Install root used by `browser_status` to locate kitesurf.
    pub kitesurf_home: PathBuf,
    pub gateway_sock: Option<PathBuf>,
    pub provision_lock: PathBuf,
    pub provision_lock_wait: Duration,
    pub lock_stale_after: Duration,
    pub connect_timeout: Duration,
    pub page_wait: Duration,
    pub idle_yield: Duration,
    pub reclaim_wait: Duration,
    pub yield_timeout: Duration,
    pub request_human_timeout: Duration,
    pub http: Option<SocketAddr>,
}

impl BrowserConfig {
    /// Reads the process environment. `http` comes from `--http`.
    pub fn from_env(http: Option<SocketAddr>) -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok(), http)
    }

    /// Environment-read seam used by the unit tests.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
        http: Option<SocketAddr>,
    ) -> Result<Self, ConfigError> {
        let owner = string_with_default(&lookup, "PERI_BROWSER_OWNER", "peri-browser")?;
        let session_name = lookup("PERI_BROWSER_SESSION_NAME")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| owner.clone());

        let kitesurf_home = match non_empty(&lookup, "KITESURF_INSTALL_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => match non_empty(&lookup, "HOME") {
                Some(home) => PathBuf::from(home).join(".kitesurf"),
                None => return Err(ConfigError::MissingHome),
            },
        };

        let temp = std::env::temp_dir();
        let gateway_sock = lookup("PERI_BROWSER_GATEWAY_SOCK")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| Some(temp.join("peri-browser-gateway.sock")));
        let provision_lock = lookup("PERI_BROWSER_PROVISION_LOCK")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| temp.join("kitesurf-provision.lock"));

        let http = match http {
            Some(address) => Some(address),
            None => match lookup("PERI_BROWSER_HTTP_ADDR")
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
            {
                Some(value) => Some(
                    value
                        .parse()
                        .map_err(|_| ConfigError::InvalidHttpAddr(value))?,
                ),
                None => None,
            },
        };

        Ok(Self {
            owner,
            session_name,
            kitesurf_home,
            gateway_sock,
            provision_lock,
            provision_lock_wait: millis(&lookup, "PERI_BROWSER_PROVISION_LOCK_WAIT_MS", 10_000)?,
            lock_stale_after: millis(&lookup, "PERI_BROWSER_LOCK_STALE_MS", 30_000)?,
            connect_timeout: millis(&lookup, "PERI_BROWSER_CONNECT_TIMEOUT_MS", 5_000)?,
            page_wait: millis(&lookup, "PERI_BROWSER_PAGE_WAIT_MS", 30_000)?,
            idle_yield: millis(&lookup, "PERI_BROWSER_IDLE_YIELD_MS", 15_000)?,
            reclaim_wait: millis(&lookup, "PERI_BROWSER_RECLAIM_WAIT_MS", 2_000)?,
            yield_timeout: millis(&lookup, "PERI_BROWSER_YIELD_TIMEOUT_MS", 60_000)?,
            request_human_timeout: millis(
                &lookup,
                "PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS",
                300_000,
            )?,
            http,
        })
    }
}

fn non_empty(lookup: &impl Fn(&str) -> Option<String>, key: &'static str) -> Option<String> {
    lookup(key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn string_with_default(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &'static str,
    default: &str,
) -> Result<String, ConfigError> {
    match lookup(key) {
        Some(value) => {
            let value = value.trim().to_owned();
            if value.is_empty() {
                Err(ConfigError::EmptyValue { key })
            } else {
                Ok(value)
            }
        }
        None => Ok(default.to_owned()),
    }
}

fn millis(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &'static str,
    default: u64,
) -> Result<Duration, ConfigError> {
    let Some(raw) = lookup(key) else {
        return Ok(Duration::from_millis(default));
    };
    let raw = raw.trim();
    let value = raw.parse::<u64>().map_err(|_| ConfigError::InvalidNumber {
        key,
        value: raw.to_owned(),
    })?;
    Ok(Duration::from_millis(value))
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
