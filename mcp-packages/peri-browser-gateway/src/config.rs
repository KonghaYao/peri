//! 配置在唯一本地 listener 启动前解析；建议端点不是已连接状态。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use uuid::Uuid;

use crate::onboarding::{install_root, normalize_endpoint, recommended_canonical};

pub(crate) const DEFAULT_BIND: &str = "127.0.0.1:7391";
pub(crate) const DEFAULT_SANDBOX_BIND: &str = "127.0.0.1:7392";
const MAX_TIMEOUT_MS: u64 = 86_400_000;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("unknown option; see --help")]
    UnknownFlag,
    #[error("{0} requires a value")]
    MissingValue(&'static str),
    #[error("bind must use 127.0.0.1 or [::1] and a port")]
    InvalidBind,
    #[error("sandbox bind must be off or a loopback IP:PORT with a non-zero port")]
    InvalidSandboxBind,
    #[error("invalid suggested CDP endpoint: {0}")]
    InvalidCdp(#[source] crate::error::BrowserError),
    #[error("{0} must be an integer from 1 to 86400000 milliseconds")]
    InvalidTimeout(&'static str),
    #[error("{0} must not be empty")]
    EmptyValue(&'static str),
    #[error("HOME is not set; set KITESURF_INSTALL_DIR")]
    MissingHome,
}

#[derive(Clone)]
pub struct Config {
    pub bind: SocketAddr,
    /// `None` disables the sandbox listener; the host page stays available.
    pub sandbox_bind: Option<SocketAddr>,
    /// Real App HTML for the sandbox listener; `None` keeps the placeholder.
    /// The file is read before any listener binds.
    pub app_html: Option<PathBuf>,
    /// Bearer token for the view-session API; `None` disables the API.
    pub(crate) view_token: Option<String>,
    pub(crate) secret: Vec<u8>,
    pub(crate) browser: BrowserConfig,
}

#[derive(Clone)]
pub(crate) struct BrowserConfig {
    pub owner: String,
    pub session_name: String,
    pub kitesurf_home: PathBuf,
    pub home: Option<String>,
    pub path: Option<String>,
    pub suggested_endpoint: String,
    pub provision_lock: PathBuf,
    pub provision_lock_wait: Duration,
    pub lock_stale_after: Duration,
    pub connect_timeout: Duration,
    pub page_wait: Duration,
    pub idle_yield: Duration,
    pub reclaim_wait: Duration,
    pub yield_timeout: Duration,
    pub request_human_timeout: Duration,
}

impl Config {
    pub fn parse_from(
        args: impl IntoIterator<Item = String>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let mut args = args.into_iter();
        let mut overrides = std::collections::HashMap::new();
        while let Some(arg) = args.next() {
            let (flag, inline) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(k, v)| (k, Some(v)));
            let key = match flag {
                "--bind" => "PERI_BROWSER_MONITOR_BIND",
                "--sandbox-bind" => "PERI_BROWSER_SANDBOX_BIND",
                "--app-html" => "PERI_BROWSER_APP_HTML",
                "--view-token" => "PERI_BROWSER_VIEW_TOKEN",
                "--cdp-url" => "KITESURF_CDP_URL",
                "--secret" => "PERI_BROWSER_GATEWAY_SECRET",
                "--idle-yield-ms" => "PERI_BROWSER_IDLE_YIELD_MS",
                "--yield-timeout-ms" => "PERI_BROWSER_YIELD_TIMEOUT_MS",
                "--request-human-timeout-ms" => "PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS",
                _ => return Err(ConfigError::UnknownFlag),
            };
            let value = inline
                .map(str::to_owned)
                .or_else(|| args.next().filter(|v| !v.starts_with("--")))
                .ok_or(ConfigError::MissingValue(key))?;
            overrides.insert(key, value);
        }
        let lookup = |key: &str| overrides.get(key).cloned().or_else(|| env(key));
        let bind = lookup("PERI_BROWSER_MONITOR_BIND")
            .as_deref()
            .unwrap_or(DEFAULT_BIND)
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::InvalidBind)?;
        if !is_bind_ip(bind.ip()) {
            return Err(ConfigError::InvalidBind);
        }
        let sandbox_bind = parse_sandbox_bind(&lookup)?;
        let app_html = match lookup("PERI_BROWSER_APP_HTML") {
            Some(value) if value.trim().is_empty() => {
                return Err(ConfigError::EmptyValue("PERI_BROWSER_APP_HTML"))
            }
            Some(value) => Some(PathBuf::from(value.trim())),
            None => None,
        };
        let suggested_endpoint = lookup("KITESURF_CDP_URL").map_or_else(
            || Ok(recommended_canonical()),
            |url| normalize_endpoint(&url).map_err(ConfigError::InvalidCdp),
        )?;
        let view_token = match lookup("PERI_BROWSER_VIEW_TOKEN") {
            Some(value) if value.trim().is_empty() => {
                return Err(ConfigError::EmptyValue("PERI_BROWSER_VIEW_TOKEN"))
            }
            Some(value) => Some(value.trim().to_owned()),
            None => None,
        };
        let secret = match lookup("PERI_BROWSER_GATEWAY_SECRET") {
            Some(value) if value.trim().is_empty() => {
                return Err(ConfigError::EmptyValue("PERI_BROWSER_GATEWAY_SECRET"))
            }
            Some(value) => value.into_bytes(),
            None => [Uuid::new_v4().into_bytes(), Uuid::new_v4().into_bytes()].concat(),
        };
        let home = env("HOME");
        let kitesurf_home = install_root(&lookup).ok_or(ConfigError::MissingHome)?;
        let owner = match lookup("PERI_BROWSER_OWNER") {
            Some(value) if value.trim().is_empty() => {
                return Err(ConfigError::EmptyValue("PERI_BROWSER_OWNER"))
            }
            Some(value) => value.trim().to_owned(),
            None => "peri-browser".to_owned(),
        };
        let browser = BrowserConfig {
            session_name: non_empty(&lookup, "PERI_BROWSER_SESSION_NAME")
                .unwrap_or_else(|| owner.clone()),
            owner,
            kitesurf_home,
            home,
            path: env("PATH"),
            suggested_endpoint,
            provision_lock: non_empty(&lookup, "PERI_BROWSER_PROVISION_LOCK")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("kitesurf-provision.lock")),
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
        };
        Ok(Self {
            bind,
            sandbox_bind,
            app_html,
            view_token,
            secret,
            browser,
        })
    }
}

impl BrowserConfig {
    pub fn page_budget(&self) -> Duration {
        self.reclaim_wait
            + self.connect_timeout
            + self.page_wait.max(Duration::from_secs(60))
            + Duration::from_secs(35)
    }

    pub fn structural_budget(&self) -> Duration {
        self.provision_lock_wait
            + self.connect_timeout
            + Duration::from_secs(20)
            + self.page_budget()
    }

    pub fn keep_alive(&self) -> Duration {
        // 每项配置有上限，因此所有加法和 Instant deadline 都在安全范围。
        let budget = self
            .structural_budget()
            .max(self.request_human_timeout)
            .max(self.yield_timeout);
        Duration::from_secs(300).max(budget + Duration::from_secs(60))
    }
}

pub(crate) fn is_bind_ip(ip: IpAddr) -> bool {
    matches!(
        ip,
        IpAddr::V4(Ipv4Addr::LOCALHOST) | IpAddr::V6(Ipv6Addr::LOCALHOST)
    )
}

/// `off`/`none` keeps the gateway listener without its sandbox source; an
/// absent value keeps the loopback default.
fn parse_sandbox_bind(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<SocketAddr>, ConfigError> {
    let Some(value) = lookup("PERI_BROWSER_SANDBOX_BIND") else {
        return DEFAULT_SANDBOX_BIND
            .parse()
            .map(Some)
            .map_err(|_| ConfigError::InvalidSandboxBind);
    };
    let value = value.trim();
    if value.eq_ignore_ascii_case("off") || value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let bind = value
        .parse::<SocketAddr>()
        .map_err(|_| ConfigError::InvalidSandboxBind)?;
    if !is_bind_ip(bind.ip()) || bind.port() == 0 {
        return Err(ConfigError::InvalidSandboxBind);
    }
    Ok(Some(bind))
}

/// kitesurf 的私有网络访问默认开启；显式关闭值同时覆盖 `--allow-private-network`。
pub(crate) fn allow_private_network(lookup: &impl Fn(&str) -> Option<String>) -> bool {
    !matches!(
        non_empty(lookup, "PERI_BROWSER_ALLOW_PRIVATE_NETWORK")
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("0" | "false" | "off" | "no")
    )
}

fn non_empty(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    lookup(key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn millis(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &'static str,
    default: u64,
) -> Result<Duration, ConfigError> {
    let value = match lookup(key) {
        Some(raw) => raw
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|n| (1..=MAX_TIMEOUT_MS).contains(n))
            .ok_or(ConfigError::InvalidTimeout(key))?,
        None => default,
    };
    Ok(Duration::from_millis(value))
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
