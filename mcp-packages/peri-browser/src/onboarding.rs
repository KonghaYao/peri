//! Runtime onboarding: endpoint normalization, install detection and the
//! guidance `browser_status` puts in front of the model.
//!
//! There is no endpoint configuration and no implicit default. The only
//! accepted endpoints are loopback HTTP/WebSocket URLs with an explicit port,
//! canonicalized to `ws://host:port/devtools/browser` so the same instance has
//! exactly one identity. Install detection never spawns a process: it checks
//! the configured install root, the default `$HOME/.kitesurf` root and `PATH`,
//! and reads the version from `current-version.txt` when it exists.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use url::Url;

use crate::error::BrowserError;
use crate::provision::probe_engine;

/// Port recommended for a local kitesurf instance. It is a convention, not a
/// default: `browser_connect` still needs the URL.
pub const RECOMMENDED_PORT: u16 = 18971;

/// Upper bound for the recommended-endpoint probe.
const RECOMMENDED_PROBE_BOUND: Duration = Duration::from_secs(1);

/// Install command handed to the model. The model must ask the user first.
pub const INSTALL_COMMAND: &str = "curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install-kitesurf.sh | bash";

/// Instruction attached while kitesurf is missing.
pub const INSTALL_INSTRUCTION: &str = "Ask the user before running installCommand.";

/// Host platform as Rust reports it, plus whether the installer covers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    pub supported: bool,
}

impl Platform {
    /// JSON shape used by `browser_status`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({"os": self.os, "arch": self.arch, "supported": self.supported})
    }
}

/// Current-process platform.
#[must_use]
pub fn platform() -> Platform {
    platform_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Platform for an explicit os/arch pair (macOS arm64 and Linux x64 only).
#[must_use]
pub fn platform_for(os: &str, arch: &str) -> Platform {
    Platform {
        os: os.to_owned(),
        arch: arch.to_owned(),
        supported: install_supported(os, arch),
    }
}

/// Whether `scripts/install-kitesurf.sh` publishes a build for this pair.
#[must_use]
pub fn install_supported(os: &str, arch: &str) -> bool {
    matches!((os, arch), ("macos", "aarch64") | ("linux", "x86_64"))
}

/// Canonical endpoint identity of the recommended local instance.
#[must_use]
pub fn recommended_endpoint() -> String {
    format!("http://127.0.0.1:{RECOMMENDED_PORT}")
}

/// Canonical WebSocket form of [`recommended_endpoint`].
#[must_use]
pub fn recommended_canonical() -> String {
    format!("ws://127.0.0.1:{RECOMMENDED_PORT}/devtools/browser")
}

/// Normalizes a user-supplied endpoint to `ws://host:port/devtools/browser`.
///
/// Accepts `http://` and `ws://` with an explicit port and a loopback host
/// (`127.0.0.0/8`, `::1`, `localhost`). The input path and query are dropped:
/// page tools build their own `/devtools/page/{id}` URLs from the canonical
/// form, and owner parameters belong to whoever attaches.
///
/// # Errors
///
/// [`BrowserError::InvalidArgument`] for TLS, a missing port, a non-loopback
/// host or an unsupported scheme.
pub fn normalize_endpoint(raw: &str) -> Result<String, BrowserError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid("url", "must not be empty"));
    }
    let url = Url::parse(trimmed).map_err(|error| invalid("url", error.to_string()))?;
    match url.scheme() {
        "http" | "ws" => {}
        "https" | "wss" => {
            return Err(invalid(
                "url",
                "TLS endpoints are not supported yet; use http:// or ws://",
            ));
        }
        scheme => {
            return Err(invalid(
                "url",
                format!("expected http:// or ws://, got {scheme}://"),
            ));
        }
    }
    let port = url.port().ok_or_else(|| {
        invalid(
            "url",
            "an explicit port is required, for example http://127.0.0.1:18971",
        )
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| invalid("url", "the URL has no host"))?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        return Err(invalid(
            "url",
            format!("host {host:?} is not loopback; use 127.0.0.0/8, ::1 or localhost"),
        ));
    }
    let host = if bare.contains(':') {
        format!("[{bare}]")
    } else {
        bare.to_owned()
    };
    Ok(format!("ws://{host}:{port}/devtools/browser"))
}

/// What install detection found. `version` is `None` when no
/// `current-version.txt` sits next to the binary (for example a mise shim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallInfo {
    pub installed: bool,
    pub version: Option<String>,
    pub path: Option<PathBuf>,
    pub on_path: bool,
}

impl InstallInfo {
    /// Not installed anywhere.
    #[must_use]
    pub fn missing() -> Self {
        Self {
            installed: false,
            version: None,
            path: None,
            on_path: false,
        }
    }

    /// JSON shape used by `browser_status`. `installInstruction` is only
    /// present while the install is missing.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "installed": self.installed,
            "version": self.version,
            "path": self.path.as_ref().map(|path| path.display().to_string()),
            "onPath": self.on_path,
            "installCommand": INSTALL_COMMAND,
        });
        if !self.installed {
            value["installInstruction"] = json!(INSTALL_INSTRUCTION);
        }
        value
    }
}

/// Looks for kitesurf in `install_dir/bin`, `$HOME/.kitesurf/bin`, then `PATH`.
///
/// `lookup` reads the process environment (`HOME`, `PATH`); it never runs a
/// subprocess, so a shim without `current-version.txt` reports `version: null`.
#[must_use]
pub fn detect_install(install_dir: &Path, lookup: &impl Fn(&str) -> Option<String>) -> InstallInfo {
    let on_path = find_on_path(lookup);
    let mut roots = vec![install_dir.to_path_buf()];
    if let Some(home) = lookup("HOME").map(|home| home.trim().to_owned()) {
        if !home.is_empty() {
            roots.push(PathBuf::from(home).join(".kitesurf"));
        }
    }
    roots.dedup();
    for root in roots {
        let candidate = root.join("bin").join("kitesurf");
        if candidate.is_file() {
            return InstallInfo {
                installed: true,
                version: read_version(&root),
                path: Some(candidate),
                on_path: on_path.is_some(),
            };
        }
    }
    match on_path {
        Some(path) => InstallInfo {
            installed: true,
            version: None,
            path: Some(path),
            on_path: true,
        },
        None => InstallInfo {
            installed: false,
            ..InstallInfo::missing()
        },
    }
}

/// Commands that take the browser from "not installed" to "connected".
///
/// States (README §7): missing install, installed but not listening, listening
/// but not connected, connected and reachable, connected but unreachable.
#[must_use]
pub fn next_steps(
    installed: bool,
    listening: bool,
    endpoint: Option<&str>,
    reachable: bool,
) -> Vec<String> {
    let serve = format!("kitesurf serve --port {RECOMMENDED_PORT}");
    let connect = format!("browser_connect(url: \"{}\")", recommended_endpoint());
    match endpoint {
        Some(endpoint) => {
            if reachable {
                Vec::new()
            } else {
                vec![serve_on(endpoint)]
            }
        }
        None if !installed => vec![INSTALL_COMMAND.to_owned(), serve, connect],
        None if !listening => vec![serve, connect],
        None => vec![connect],
    }
}

/// Whether the recommended local endpoint answers `/json/version`.
///
/// Only the loopback address is probed, bounded to one second, and only while
/// no endpoint is connected.
pub async fn probe_recommended(connect_timeout: Duration) -> bool {
    let bound = connect_timeout.min(RECOMMENDED_PROBE_BOUND);
    matches!(
        tokio::time::timeout(bound, probe_engine(&recommended_canonical(), bound)).await,
        Ok(probe) if probe.reachable
    )
}

fn invalid(name: &str, reason: impl Into<String>) -> BrowserError {
    BrowserError::InvalidArgument {
        name: name.to_owned(),
        reason: reason.into(),
    }
}

fn serve_on(endpoint: &str) -> String {
    let port = Url::parse(endpoint)
        .ok()
        .and_then(|url| url.port())
        .unwrap_or(RECOMMENDED_PORT);
    format!("kitesurf serve --port {port}")
}

fn read_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("current-version.txt")).ok()?;
    let version = text.trim();
    (!version.is_empty()).then(|| version.to_owned())
}

fn find_on_path(lookup: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let path = lookup("PATH")?;
    path.split(':')
        .filter(|entry| !entry.is_empty())
        .map(|entry| Path::new(entry).join("kitesurf"))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
#[path = "onboarding_test.rs"]
mod tests;
