//! `install kitesurf`, `update kitesurf` and the engine side of `start
//! kitesurf`.
//!
//! Installation downloads the official script into a private temporary
//! directory and runs it with `bash`, so a failed `curl` cannot be hidden by a
//! pipe and no other user can plant the file. Starting spawns
//! `kitesurf serve` in its own process group and waits for `/json/version`;
//! the engine keeps running after the command that started it exits.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::config::allow_private_network;
use crate::onboarding::{detect_install, install_root};
use crate::provision::probe_engine;

/// Official installer for kitesurf releases.
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/konghayao/peri/main/scripts/install-kitesurf.sh";

/// Bound for the engine readiness probe.
const ENGINE_READY_LIMIT: Duration = Duration::from_secs(15);
/// One probe attempt stays short so a refused or stalled connect does not eat
/// the whole readiness budget.
const PROBE_ATTEMPT_LIMIT: Duration = Duration::from_millis(500);
/// Readiness poll interval.
const READY_POLL: Duration = Duration::from_millis(100);

/// Directory name attempts before giving up; each one is a fresh name.
const INSTALLER_DIR_ATTEMPTS: u64 = 16;
/// Owner-only mode for the directory holding the downloaded script.
const INSTALLER_DIR_MODE: u32 = 0o700;
/// Script name inside the private directory.
const INSTALLER_SCRIPT_NAME: &str = "install-kitesurf.sh";

/// Mixed into the directory name so repeated installs in the same process never
/// ask for the same path.
static INSTALLER_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

/// `install kitesurf`: report an existing install or run the official script.
///
/// Returns the installer's exit code so callers can propagate it unchanged.
pub fn install(lookup: &impl Fn(&str) -> Option<String>) -> Result<i32> {
    install_or_update(lookup, false)
}

/// `update kitesurf`: re-run the official installer unconditionally.
///
/// The installer resolves and downloads the latest release itself, so running
/// it again is the update; skipping the already-installed short circuit is
/// what makes `update` differ from the idempotent `install`. The installer
/// replaces symlinks only and never touches a running process, so a running
/// engine keeps the old binary until it is restarted.
pub fn update_kitesurf(lookup: &impl Fn(&str) -> Option<String>) -> Result<i32> {
    install_or_update(lookup, true)
}

/// Shared install path; `force` skips the already-installed short circuit.
fn install_or_update(lookup: &impl Fn(&str) -> Option<String>, force: bool) -> Result<i32> {
    let root = install_root(lookup).context("HOME is not set; set KITESURF_INSTALL_DIR")?;
    let info = detect_install(&root, lookup);
    if force {
        match (info.installed, info.version.as_deref()) {
            (true, Some(version)) => println!("updating kitesurf {version} ..."),
            (true, None) => println!("updating kitesurf ..."),
            (false, _) => println!("kitesurf is not installed; installing the latest release"),
        }
        return run_installer();
    }
    if info.installed {
        match (info.path.as_ref(), info.version.as_ref()) {
            (Some(path), Some(version)) => println!(
                "kitesurf already installed: {} (version {version})",
                path.display()
            ),
            (Some(path), None) => {
                println!("kitesurf already installed: {}", path.display());
            }
            (None, _) => println!("kitesurf already installed"),
        }
        return Ok(0);
    }
    run_installer()
}

/// `start kitesurf`: reuse or spawn the engine and wait until it answers.
///
/// Returns `true` when this call spawned the engine, so the caller records
/// ownership in the gateway state and `stop` knows whether the engine is ours.
pub async fn ensure_engine(port: u16, lookup: &impl Fn(&str) -> Option<String>) -> Result<bool> {
    let engine_url = engine_url(port);
    if probe_engine(&engine_probe_url(port), PROBE_ATTEMPT_LIMIT)
        .await
        .reachable
    {
        println!("kitesurf already running at {engine_url}");
        return Ok(false);
    }
    start_engine(port, lookup).await?;
    Ok(true)
}

/// HTTP origin shown to the user.
fn engine_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Canonical CDP form for probes: `probe_engine` deliberately refuses plain
/// HTTP endpoints (see `provision_test`), and `/json/version` is derived from
/// the WebSocket endpoint the same way `browser_connect` derives it.
fn engine_probe_url(port: u16) -> String {
    format!("ws://127.0.0.1:{port}/devtools/browser")
}

/// Creates an owner-only directory for the downloaded installer.
///
/// `create_dir` is atomic and refuses an existing entry, so a file or symlink
/// planted under the same name in a shared temp directory makes this pick the
/// next name instead of writing through it.
fn make_installer_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    for _ in 0..INSTALLER_DIR_ATTEMPTS {
        let seq = INSTALLER_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("peri-browser-install-kitesurf-{pid}-{seq}"));
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("create {}", dir.display()));
            }
        }
        if let Err(error) =
            fs::set_permissions(&dir, fs::Permissions::from_mode(INSTALLER_DIR_MODE))
        {
            let _ = fs::remove_dir_all(&dir);
            return Err(error).with_context(|| format!("restrict {}", dir.display()));
        }
        return Ok(dir);
    }
    bail!(
        "no unused installer directory name under {}",
        base.display()
    )
}

/// Reserves the script path inside `dir` with `create_new`, so a pre-existing
/// file cannot be reused as the download target.
fn create_installer_script(dir: &Path) -> Result<PathBuf> {
    let script = dir.join(INSTALLER_SCRIPT_NAME);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&script)
        .with_context(|| format!("create {}", script.display()))?;
    Ok(script)
}

/// Removes the whole installer directory; safe to call twice.
fn cleanup_installer_dir(dir: &Path) {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => eprintln!("warning: could not remove {}: {error}", dir.display()),
    }
}

fn run_installer() -> Result<i32> {
    let dir = make_installer_dir()?;
    let result = run_installer_in(&dir);
    cleanup_installer_dir(&dir);
    result
}

/// Downloads the script into `dir` and runs it, so the caller owns cleanup on
/// every path.
fn run_installer_in(dir: &Path) -> Result<i32> {
    let script = create_installer_script(dir)?;
    match Command::new("curl")
        .args(["-fsSL", "--output"])
        .arg(&script)
        .arg(INSTALL_SCRIPT_URL)
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => return Ok(exit_code(status)),
        Err(error) => {
            return Err(error).context("run curl to download the kitesurf installer");
        }
    }
    match Command::new("bash").arg(&script).status() {
        Ok(status) => Ok(exit_code(status)),
        Err(error) => Err(error).context("run the kitesurf installer with bash"),
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

async fn start_engine(port: u16, lookup: &impl Fn(&str) -> Option<String>) -> Result<()> {
    let root = install_root(lookup).context("HOME is not set; set KITESURF_INSTALL_DIR")?;
    let info = detect_install(&root, lookup);
    if !info.installed {
        bail!("kitesurf is not installed; run `peri-browser install kitesurf` first");
    }
    let binary = info
        .path
        .ok_or_else(|| anyhow::anyhow!("kitesurf is installed but its binary path is unknown"))?;
    let log = root.join(format!("serve-{port}.log"));
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create install root {}", root.display()))?;
    let mut child = spawn_engine(&binary, port, &log, allow_private_network(lookup))?;
    let url = engine_probe_url(port);
    match wait_for_engine(&url, &mut child).await {
        Ok(()) => {
            println!(
                "kitesurf started at {} (log: {})",
                engine_url(port),
                log.display()
            );
            Ok(())
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
                .with_context(|| format!("kitesurf did not become ready; see {}", log.display()))
        }
    }
}

fn spawn_engine(
    binary: &Path,
    port: u16,
    log: &Path,
    allow_private_network: bool,
) -> Result<Child> {
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("open {}", log.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("duplicate {}", log.display()))?;
    let mut command = Command::new(binary);
    command
        .args(engine_args(port, allow_private_network))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .process_group(0);
    command
        .spawn()
        .with_context(|| format!("start {}", binary.display()))
}

/// `kitesurf serve` arguments; the private-network flag is omitted only when the
/// operator disables it through `PERI_BROWSER_ALLOW_PRIVATE_NETWORK`.
fn engine_args(port: u16, allow_private_network: bool) -> Vec<String> {
    let mut args = vec!["serve".to_owned(), "--port".to_owned(), port.to_string()];
    if allow_private_network {
        args.push("--allow-private-network".to_owned());
    }
    args
}

async fn wait_for_engine(url: &str, child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + ENGINE_READY_LIMIT;
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("kitesurf exited before becoming ready ({status})");
        }
        if probe_engine(url, PROBE_ATTEMPT_LIMIT).await.reachable {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out after {}s waiting for {url}",
                ENGINE_READY_LIMIT.as_secs()
            );
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
