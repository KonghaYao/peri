//! Gateway configuration: CLI flags, environment fallbacks and defaults.
//!
//! Parsing is explicit (no clap) and takes the environment as a closure so
//! tests never touch process-global state.

use std::io::{ErrorKind, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use uuid::Uuid;

pub const CDP_URL_ENV: &str = "KITESURF_CDP_URL";
pub const BIND_ENV: &str = "PERI_BROWSER_MONITOR_BIND";
pub const REGISTRY_SOCK_ENV: &str = "PERI_BROWSER_GATEWAY_SOCK";
pub const SECRET_ENV: &str = "PERI_BROWSER_GATEWAY_SECRET";
pub const IDLE_YIELD_ENV: &str = "PERI_BROWSER_IDLE_YIELD_MS";
pub const YIELD_TIMEOUT_ENV: &str = "PERI_BROWSER_YIELD_TIMEOUT_MS";
pub const REQUEST_HUMAN_TIMEOUT_ENV: &str = "PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS";

/// Fixed loopback port shared by every Peri session (README §6.1).
pub const DEFAULT_BIND: &str = "127.0.0.1:7391";
pub const DEFAULT_SOCK_NAME: &str = "peri-browser-gateway.sock";
/// 32 random bytes rendered as hex.
pub const SECRET_HEX_LEN: usize = 64;

/// Idle agent → yield the controller to the monitor (README §6.2).
pub const DEFAULT_IDLE_YIELD_MS: u64 = 15_000;
/// How long a manual takeover waits for the MCP to finish its call.
pub const DEFAULT_YIELD_TIMEOUT_MS: u64 = 5_000;
/// How long a `request_human` handoff waits for a human answer.
pub const DEFAULT_REQUEST_HUMAN_TIMEOUT_MS: u64 = 120_000;
/// Upper bound for any of the three timeouts: a day is long past "sane".
pub const MAX_TIMEOUT_MS: u64 = 86_400_000;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required --cdp-url or {0}")]
    MissingCdpUrl(&'static str),
    #[error("unknown argument {0}")]
    UnknownFlag(String),
    #[error("{flag} requires a value")]
    MissingValue { flag: String },
    #[error("invalid CDP url {value:?}: expected ws://host:port or wss://host:port")]
    InvalidCdpUrl { value: String },
    #[error("invalid bind address {value:?}: {reason}")]
    InvalidBind { value: String, reason: String },
    #[error("{flag} needs {reason}: got {value:?}")]
    InvalidTimeout {
        flag: &'static str,
        value: String,
        reason: String,
    },
    #[error("{source_name} must not be empty")]
    EmptySecret { source_name: &'static str },
    #[error("registry socket path must not be empty")]
    EmptySocketPath,
    #[error("{flag} does not take a value")]
    UnexpectedValue { flag: String },
    #[error("cannot read gateway secret {path}: {source}")]
    SecretFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("gateway secret file {path} is empty")]
    EmptySecretFile { path: PathBuf },
}

/// Gateway secret used to derive session tokens.
///
/// The value is never printed, serialized or logged: `Debug` only says whether
/// it is set.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    fn from_value(value: String, source_name: &'static str) -> Result<Self, ConfigError> {
        if value.is_empty() {
            return Err(ConfigError::EmptySecret { source_name });
        }
        Ok(Self(value.into_bytes()))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

/// Fully resolved runtime configuration.
#[derive(Clone, Debug)]
pub struct Config {
    pub cdp_url: String,
    pub bind: SocketAddr,
    pub registry_sock: PathBuf,
    /// Explicit secret from `--secret` / `PERI_BROWSER_GATEWAY_SECRET`.
    ///
    /// `None` means "load or create the persisted secret next to the registry
    /// socket" via [`Config::resolve_secret`]. Parsing stays side-effect free so
    /// tests never write files.
    pub secret: Option<Secret>,
    /// Standalone development mode: discover pages from the Kitesurf HTTP
    /// endpoint instead of waiting for an MCP registration.
    pub dev: bool,
    /// Agent idle time after which the gateway asks for the controller.
    pub idle_yield_ms: u64,
    /// Manual takeover wait before the viewer is told the yield is pending.
    pub yield_timeout_ms: u64,
    /// Human handoff wait before the MCP receives `timeout`.
    pub request_human_timeout_ms: u64,
}

impl Config {
    /// Parses `args` (without the program name), falling back to `env`.
    ///
    /// Precedence per setting: CLI flag, then environment, then default. The
    /// secret is generated when neither source provides one.
    pub fn parse_from<I, F>(args: I, env: F) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = String>,
        F: Fn(&str) -> Option<String>,
    {
        let mut args = args.into_iter();
        let mut cdp_url = None;
        let mut bind = None;
        let mut registry_sock = None;
        let mut secret = None;
        let mut dev = false;
        let mut idle_yield = None;
        let mut yield_timeout = None;
        let mut request_human_timeout = None;

        while let Some(argument) = args.next() {
            let (flag, inline) = match argument.split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => {
                    (flag.to_owned(), Some(value.to_owned()))
                }
                _ => (argument, None),
            };

            match flag.as_str() {
                "--cdp-url" => cdp_url = Some(take_value(&flag, inline, &mut args)?),
                "--bind" => bind = Some(take_value(&flag, inline, &mut args)?),
                "--registry-sock" => {
                    registry_sock = Some(take_value(&flag, inline, &mut args)?);
                }
                "--secret" => secret = Some(take_value(&flag, inline, &mut args)?),
                "--idle-yield-ms" => idle_yield = Some(take_value(&flag, inline, &mut args)?),
                "--yield-timeout-ms" => {
                    yield_timeout = Some(take_value(&flag, inline, &mut args)?);
                }
                "--request-human-timeout-ms" => {
                    request_human_timeout = Some(take_value(&flag, inline, &mut args)?);
                }
                "--dev" => {
                    if inline.is_some() {
                        return Err(ConfigError::UnexpectedValue { flag });
                    }
                    dev = true;
                }
                other => return Err(ConfigError::UnknownFlag(other.to_owned())),
            }
        }

        let cdp_url = cdp_url
            .or_else(|| env(CDP_URL_ENV))
            .ok_or(ConfigError::MissingCdpUrl(CDP_URL_ENV))?;
        validate_cdp_url(&cdp_url)?;

        let bind_value = bind
            .or_else(|| env(BIND_ENV))
            .unwrap_or_else(|| DEFAULT_BIND.to_owned());
        let bind = bind_value
            .parse()
            .map_err(|error| ConfigError::InvalidBind {
                value: bind_value.clone(),
                reason: format!("{error}"),
            })?;

        let registry_sock = registry_sock
            .or_else(|| env(REGISTRY_SOCK_ENV))
            .map(PathBuf::from)
            .unwrap_or_else(default_registry_sock);
        if registry_sock.as_os_str().is_empty() {
            return Err(ConfigError::EmptySocketPath);
        }

        let secret = match secret.or_else(|| env(SECRET_ENV)) {
            Some(value) => Some(Secret::from_value(
                value,
                "--secret / PERI_BROWSER_GATEWAY_SECRET",
            )?),
            None => None,
        };

        let idle_yield_ms = parse_timeout(
            "--idle-yield-ms / PERI_BROWSER_IDLE_YIELD_MS",
            idle_yield.or_else(|| env(IDLE_YIELD_ENV)),
            DEFAULT_IDLE_YIELD_MS,
        )?;
        let yield_timeout_ms = parse_timeout(
            "--yield-timeout-ms / PERI_BROWSER_YIELD_TIMEOUT_MS",
            yield_timeout.or_else(|| env(YIELD_TIMEOUT_ENV)),
            DEFAULT_YIELD_TIMEOUT_MS,
        )?;
        let request_human_timeout_ms = parse_timeout(
            "--request-human-timeout-ms / PERI_BROWSER_REQUEST_HUMAN_TIMEOUT_MS",
            request_human_timeout.or_else(|| env(REQUEST_HUMAN_TIMEOUT_ENV)),
            DEFAULT_REQUEST_HUMAN_TIMEOUT_MS,
        )?;

        Ok(Self {
            cdp_url,
            bind,
            registry_sock,
            secret,
            dev,
            idle_yield_ms,
            yield_timeout_ms,
            request_human_timeout_ms,
        })
    }

    /// Secret file used when neither `--secret` nor the environment supplies
    /// one. It is a sibling of the registry socket so a gateway restart with
    /// the same socket keeps every derived monitor URL valid (README §6.1).
    #[must_use]
    pub fn secret_path(&self) -> PathBuf {
        self.registry_sock.with_extension("secret")
    }

    /// Returns the explicit secret or loads/creates the persisted one.
    ///
    /// The generated file is written with mode `0600`; its content is never
    /// logged. Two gateways racing on the same socket both read the winner's
    /// file, so they agree on tokens.
    pub fn resolve_secret(&self) -> Result<Secret, ConfigError> {
        match &self.secret {
            Some(secret) => Ok(secret.clone()),
            None => {
                let path = self.secret_path();
                let value = load_or_create_secret(&path)?;
                Ok(Secret(value.into_bytes()))
            }
        }
    }

    /// HTTP base of the Kitesurf endpoint, used by development discovery.
    ///
    /// `ws://host:port/devtools/browser` and `wss://host:port/...` map to
    /// `http://host:port` and `https://host:port`; `/json/list` is mounted at
    /// the root, not below `/devtools`.
    #[must_use]
    pub fn dev_http_base(&self) -> String {
        let (scheme, rest) = match self.cdp_url.strip_prefix("ws://") {
            Some(rest) => ("http", rest),
            None => (
                "https",
                self.cdp_url.strip_prefix("wss://").unwrap_or(&self.cdp_url),
            ),
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        format!("{scheme}://{authority}")
    }

    /// Base URL used in logs and in the session list.
    ///
    /// An unspecified bind (`0.0.0.0`, `[::]`) is advertised as loopback: the
    /// operator knows the machine address, the log does not.
    #[must_use]
    pub fn monitor_base_url(&self) -> String {
        let ip = if self.bind.ip().is_unspecified() {
            match self.bind {
                SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            }
        } else {
            self.bind.ip()
        };
        format!("http://{}", SocketAddr::new(ip, self.bind.port()))
    }

    /// Full monitor URL for one session token.
    #[must_use]
    pub fn session_url(&self, token: &str) -> String {
        format!("{}/s/{token}/", self.monitor_base_url())
    }
}

/// Default registry socket inside the OS temporary directory.
#[must_use]
pub fn default_registry_sock() -> PathBuf {
    std::env::temp_dir().join(DEFAULT_SOCK_NAME)
}

/// Generates a 64-character hex secret from 32 random bytes.
///
/// Two UUID v4 values supply the entropy; UUID v4 is the random source already
/// available in this process, so no extra RNG dependency is pulled in.
#[must_use]
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    for chunk in bytes.chunks_mut(16) {
        chunk.copy_from_slice(Uuid::new_v4().as_bytes());
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(SECRET_HEX_LEN);
    for byte in bytes {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Reads the persisted secret, creating it on first use.
///
/// `create_new` makes the creation atomic between gateways; the loser of the
/// race re-reads the winner's file instead of overwriting it.
fn load_or_create_secret(path: &Path) -> Result<String, ConfigError> {
    for _ in 0..8 {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(ConfigError::EmptySecretFile {
                        path: path.to_owned(),
                    });
                }
                return Ok(trimmed.to_owned());
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let generated = generate_secret();
                match create_secret_file(path, &generated) {
                    Ok(()) => return Ok(generated),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                    Err(source) => {
                        return Err(ConfigError::SecretFile {
                            path: path.to_owned(),
                            source,
                        });
                    }
                }
            }
            Err(source) => {
                return Err(ConfigError::SecretFile {
                    path: path.to_owned(),
                    source,
                });
            }
        }
    }
    Err(ConfigError::SecretFile {
        path: path.to_owned(),
        source: std::io::Error::new(
            ErrorKind::AlreadyExists,
            "secret file kept changing while it was being created",
        ),
    })
}

fn create_secret_file(path: &Path, secret: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(secret.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

fn take_value(
    flag: &str,
    inline: Option<String>,
    args: &mut impl Iterator<Item = String>,
) -> Result<String, ConfigError> {
    match inline {
        Some(value) => Ok(value),
        None => args.next().ok_or_else(|| ConfigError::MissingValue {
            flag: flag.to_owned(),
        }),
    }
}

/// Resolves one timeout from CLI/env/default and validates it.
///
/// Zero would make a timer fire immediately and a day is already far beyond a
/// usable interaction, so both ends are rejected instead of silently clamped.
fn parse_timeout(
    flag: &'static str,
    value: Option<String>,
    default: u64,
) -> Result<u64, ConfigError> {
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u64>()
        .map_err(|error| ConfigError::InvalidTimeout {
            flag,
            value: value.clone(),
            reason: format!("a whole number of milliseconds ({error})"),
        })?;
    if parsed == 0 || parsed > MAX_TIMEOUT_MS {
        return Err(ConfigError::InvalidTimeout {
            flag,
            value,
            reason: format!("a value between 1 and {MAX_TIMEOUT_MS}"),
        });
    }
    Ok(parsed)
}

fn validate_cdp_url(value: &str) -> Result<(), ConfigError> {
    let authority = value
        .strip_prefix("ws://")
        .or_else(|| value.strip_prefix("wss://"));
    match authority {
        Some(authority) if !authority.is_empty() && !authority.starts_with('/') => Ok(()),
        _ => Err(ConfigError::InvalidCdpUrl {
            value: value.to_owned(),
        }),
    }
}

/// Removes a stale socket file so a crashed gateway does not block startup.
///
/// A successful connect means another gateway is alive; that is a startup
/// error, not something to paper over by unlinking the live socket.
pub fn prepare_registry_socket(path: &Path) -> std::io::Result<()> {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("registry socket {} is already in use", path.display()),
        )),
        Err(_) => {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
