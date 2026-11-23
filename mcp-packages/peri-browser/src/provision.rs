//! Page create/close path over the short `/devtools/browser` connection.
//!
//! Kitesurf keeps one controller for the browser endpoint, so target creation
//! and removal are serialised by a machine-local lock file around the short
//! connection. Page commands never use this connection (README §4).

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::error::BrowserError;

/// How often a waiting session retries the lock.
const LOCK_POLL: Duration = Duration::from_millis(50);
/// Upper bound for one browser-endpoint command reply.
const BROWSER_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound for an HTTP probe response.
const HTTP_PROBE_LIMIT: usize = 2 * 1024 * 1024;

/// Exclusive create lock with stale detection and a bounded wait.
#[derive(Debug)]
pub struct ProvisionLock {
    path: PathBuf,
    _file: File,
}

impl ProvisionLock {
    /// Acquires the lock, waiting at most `wait`.
    ///
    /// A lock file older than `stale_after` is removed: a crashed session must
    /// not block every later create/close. The lock records pid and time so a
    /// human can inspect it.
    ///
    /// # Errors
    ///
    /// [`BrowserError::LockTimeout`] when the wait bound expires;
    /// [`BrowserError::Internal`] for an unusable lock path.
    pub async fn acquire(
        path: &Path,
        wait: Duration,
        stale_after: Duration,
    ) -> Result<Self, BrowserError> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    let _ = writeln!(
                        file,
                        "pid={}\nstarted_unix_ms={}",
                        std::process::id(),
                        unix_millis()
                    );
                    return Ok(Self {
                        path: path.to_path_buf(),
                        _file: file,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(path, stale_after) {
                        // Another waiter may remove it first; ignore that.
                        let _ = std::fs::remove_file(path);
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(BrowserError::LockTimeout(path.to_path_buf()));
                    }
                    tokio::time::sleep(LOCK_POLL).await;
                }
                Err(error) => {
                    return Err(BrowserError::Internal(format!(
                        "cannot create provision lock {}: {error}",
                        path.display()
                    )));
                }
            }
        }
    }
}

impl Drop for ProvisionLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn is_stale(path: &Path, stale_after: Duration) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age > stale_after)
        .unwrap_or(false)
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

/// Settings for one short browser-endpoint operation.
#[derive(Debug, Clone)]
pub struct ProvisionSettings {
    pub cdp_url: String,
    pub owner: String,
    pub lock: PathBuf,
    pub lock_wait: Duration,
    pub stale_after: Duration,
    pub connect_timeout: Duration,
}

impl ProvisionSettings {
    /// Builds the browser endpoint URL, preserving any configured query.
    ///
    /// # Errors
    ///
    /// [`BrowserError::InvalidArgument`] when the endpoint is not a WebSocket URL.
    pub fn browser_url(&self) -> Result<String, BrowserError> {
        browser_ws_url(&self.cdp_url, &self.owner)
    }
}

/// Creates a blank target and returns its target id.
///
/// The URL is deliberately `about:blank`: Kitesurf does not load a real URL
/// passed to `Target.createTarget`, the controller navigates afterwards.
///
/// # Errors
///
/// Propagates lock and browser-endpoint failures.
pub async fn create_target(settings: &ProvisionSettings) -> Result<String, BrowserError> {
    let _lock =
        ProvisionLock::acquire(&settings.lock, settings.lock_wait, settings.stale_after).await?;
    let mut session = BrowserSession::open(settings).await?;
    let result = session
        .command("Target.createTarget", json!({"url": "about:blank"}))
        .await?;
    result
        .get("targetId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| BrowserError::BrowserControl {
            method: "Target.createTarget".to_owned(),
            message: "reply has no targetId".to_owned(),
        })
}

/// Closes a target through the short browser connection.
///
/// Never sent on the page connection: Kitesurf closes that socket with
/// `1008 Target expired` and never answers the command (README §7).
///
/// # Errors
///
/// Propagates lock and browser-endpoint failures.
pub async fn close_target(
    settings: &ProvisionSettings,
    target_id: &str,
) -> Result<(), BrowserError> {
    let _lock =
        ProvisionLock::acquire(&settings.lock, settings.lock_wait, settings.stale_after).await?;
    let mut session = BrowserSession::open(settings).await?;
    let result = session
        .command("Target.closeTarget", json!({"targetId": target_id}))
        .await?;
    if result.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(BrowserError::BrowserControl {
            method: "Target.closeTarget".to_owned(),
            message: format!("target {target_id} was not closed"),
        });
    }
    Ok(())
}

struct BrowserSession {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: u64,
}

impl BrowserSession {
    async fn open(settings: &ProvisionSettings) -> Result<Self, BrowserError> {
        let url = settings.browser_url()?;
        let socket = tokio::time::timeout(
            settings.connect_timeout,
            tokio_tungstenite::connect_async(&url),
        )
        .await
        .map_err(|_| {
            BrowserError::Timeout(format!(
                "connecting to the browser endpoint timed out after {:?}",
                settings.connect_timeout
            ))
        })?
        .map_err(|error| BrowserError::BrowserControl {
            method: "WebSocket connect".to_owned(),
            message: error.to_string(),
        })?;
        Ok(Self {
            socket: socket.0,
            next_id: 1,
        })
    }

    async fn command(&mut self, method: &str, params: Value) -> Result<Value, BrowserError> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({"id": id, "method": method, "params": params});
        self.socket
            .send(Message::Text(payload.to_string().into()))
            .await
            .map_err(|error| BrowserError::BrowserControl {
                method: method.to_owned(),
                message: error.to_string(),
            })?;

        let deadline = tokio::time::Instant::now() + BROWSER_COMMAND_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(BrowserError::Timeout(format!(
                    "{method} on the browser endpoint timed out"
                )));
            }
            let message = match tokio::time::timeout(remaining, self.socket.next()).await {
                Ok(Some(Ok(message))) => message,
                Ok(Some(Err(error))) => {
                    return Err(BrowserError::BrowserControl {
                        method: method.to_owned(),
                        message: error.to_string(),
                    });
                }
                Ok(None) => {
                    return Err(BrowserError::BrowserControl {
                        method: method.to_owned(),
                        message: "browser endpoint closed the connection".to_owned(),
                    });
                }
                Err(_) => {
                    return Err(BrowserError::Timeout(format!(
                        "{method} on the browser endpoint timed out"
                    )));
                }
            };
            let Message::Text(text) = message else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(BrowserError::BrowserControl {
                    method: method.to_owned(),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown browser endpoint error")
                        .to_owned(),
                });
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

/// Builds `ws://host:port/devtools/browser?owner=...`.
///
/// # Errors
///
/// [`BrowserError::InvalidArgument`] for a non-WebSocket endpoint.
pub fn browser_ws_url(cdp_url: &str, owner: &str) -> Result<String, BrowserError> {
    let mut url = Url::parse(cdp_url).map_err(|error| BrowserError::InvalidArgument {
        name: "cdp_url".to_owned(),
        reason: error.to_string(),
    })?;
    match url.scheme() {
        "ws" | "wss" => {}
        scheme => {
            return Err(BrowserError::InvalidArgument {
                name: "cdp_url".to_owned(),
                reason: format!("expected ws:// or wss://, got {scheme}://"),
            });
        }
    }
    url.set_path("/devtools/browser");
    url.query_pairs_mut().append_pair("owner", owner);
    Ok(url.to_string())
}

/// Reachability snapshot used by `browser_status`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineProbe {
    pub reachable: bool,
    pub version: Option<Value>,
    pub page_count: Option<usize>,
    pub error: Option<String>,
}

/// Reads `/json/version` and `/json/list` over the loopback HTTP surface.
///
/// This is best-effort: browser tools still work from the WebSocket endpoint
/// when the HTTP probe is unavailable.
pub async fn probe_engine(cdp_url: &str, timeout: Duration) -> EngineProbe {
    let version = match http_json(cdp_url, "/json/version", timeout).await {
        Ok(version) => version,
        Err(error) => {
            return EngineProbe {
                reachable: false,
                version: None,
                page_count: None,
                error: Some(error),
            };
        }
    };
    let page_count = match http_json(cdp_url, "/json/list", timeout).await {
        Ok(list) => Some(list.as_array().map_or(0, Vec::len)),
        Err(error) => {
            return EngineProbe {
                reachable: true,
                version: Some(version),
                page_count: None,
                error: Some(error),
            };
        }
    };
    EngineProbe {
        reachable: true,
        version: Some(version),
        page_count,
        error: None,
    }
}

async fn http_json(cdp_url: &str, path: &str, timeout: Duration) -> Result<Value, String> {
    let mut url = Url::parse(cdp_url).map_err(|error| error.to_string())?;
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => {
            return Err("TLS CDP endpoints cannot be probed over plain HTTP".to_owned());
        }
        other => return Err(format!("unsupported CDP scheme {other}://")),
    };
    let host = url
        .host_str()
        .ok_or_else(|| "CDP endpoint has no host".to_owned())?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = url.port_or_known_default().unwrap_or(80);
    url.set_scheme(scheme).ok();
    let host_header = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };

    let exchange = async {
        let mut stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| error.to_string())?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        let mut response = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let read = stream
                .read(&mut chunk)
                .await
                .map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            response.extend_from_slice(&chunk[..read]);
            if response.len() > HTTP_PROBE_LIMIT {
                return Err("HTTP probe response is too large".to_owned());
            }
        }
        parse_http_json(&response)
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| format!("probe of {path} timed out after {timeout:?}"))?
}

fn parse_http_json(response: &[u8]) -> Result<Value, String> {
    let text = String::from_utf8_lossy(response);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response".to_owned())?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| "malformed HTTP status line".to_owned())?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}"));
    }
    let body = if head.lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    }) {
        decode_chunked(body)?
    } else {
        body.to_owned()
    };
    serde_json::from_str(&body).map_err(|error| format!("invalid JSON probe response: {error}"))
}

fn decode_chunked(body: &str) -> Result<String, String> {
    let mut rest = body;
    let mut decoded = String::new();
    loop {
        let (size_line, after_size) = rest
            .split_once("\r\n")
            .ok_or_else(|| "truncated chunked body".to_owned())?;
        let size = usize::from_str_radix(size_line.trim(), 16)
            .map_err(|_| "invalid chunk size".to_owned())?;
        if size == 0 {
            break;
        }
        if after_size.len() < size {
            return Err("truncated chunk".to_owned());
        }
        decoded.push_str(&after_size[..size]);
        rest = after_size[size..]
            .strip_prefix("\r\n")
            .ok_or_else(|| "missing chunk terminator".to_owned())?;
    }
    Ok(decoded)
}

/// Best-effort check that the monitor gateway UDS is accepting connections.
pub async fn gateway_available(path: &Path, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, tokio::net::UnixStream::connect(path)).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
#[path = "provision_test.rs"]
mod tests;
