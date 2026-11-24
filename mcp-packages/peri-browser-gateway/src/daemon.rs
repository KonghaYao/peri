//! Background gateway lifecycle: `start` and `stop`.
//!
//! `serve` stays in the foreground. `start` re-executes this binary as
//! `serve`, records a per-bind state file, and returns once `/healthz`
//! answers. `stop` finds exactly the process recorded for a bind and
//! terminates it, then stops the engine only when this gateway owned it and no
//! other state file references the same engine port.

use std::fs::{self, DirBuilder, OpenOptions};
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::config::Config;
use crate::onboarding::{detect_install, install_root};

/// Budget for the child to answer `/healthz` after it is spawned.
const HEALTH_LIMIT: Duration = Duration::from_secs(10);
/// Health-check poll interval.
const HEALTH_POLL: Duration = Duration::from_millis(100);
/// One `/healthz` attempt stays short so a stalled connect cannot eat the
/// whole readiness budget.
const HEALTH_ATTEMPT: Duration = Duration::from_millis(500);
/// Cap on the `/healthz` response so a misbehaving listener cannot grow the
/// readiness check without bound.
const HEALTH_RESPONSE_LIMIT: usize = 64 * 1024;
/// SIGTERM grace period for `stop` and for a failed `start`.
const STOP_LIMIT: Duration = Duration::from_secs(5);
/// SIGTERM poll interval.
const STOP_POLL: Duration = Duration::from_millis(100);
/// Owner-only mode for `$HOME/.peri/browser/run` and `.../logs`.
const DIR_MODE: u32 = 0o700;
/// Kernel `comm` basename of this binary; the daemon must not kill anything else.
const GATEWAY_COMM: &str = "peri-browser";

/// Engine a background gateway was started against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineBinding {
    pub port: u16,
    /// `true` when this `peri-browser start` spawned the engine itself.
    pub owned: bool,
}

/// One running background gateway, as stored in `run/gateway-<port>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayState {
    pub pid: u32,
    /// OS start identity captured once at spawn; absent in old state files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start: Option<String>,
    pub bind: String,
    pub log: String,
    pub engine_port: Option<u16>,
    pub engine_owned: bool,
    pub started_at: u64,
}

/// Result of trying to claim `run/gateway-<port>.json` without overwriting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    /// This caller created the state file.
    Published,
    /// Another caller already published; the existing file was left alone.
    AlreadyExists,
}

/// What `start` should do with an existing state file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartDecision {
    /// No state, or the recorded pid is dead or reused.
    Start,
    /// A state file exists but no longer names a live gateway.
    CleanStale,
    /// A live gateway already owns this bind.
    AlreadyRunning { pid: u32 },
}

/// What `stop` should do with the recorded process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopDecision {
    /// No state, or the recorded pid is dead.
    NotRunning,
    /// A live gateway exists on this port for another loopback address.
    OtherBind,
    /// The pid lives but is not a `peri-browser` process: never kill it.
    Refuse { pid: u32, comm: String },
    /// The pid is this binary: send SIGTERM.
    Terminate { pid: u32 },
}

/// `start`: run the gateway in the background and return once it is healthy.
///
/// `args` are the `serve` options unchanged; they are replayed to the child
/// without the `start` subcommand.
pub async fn start_gateway(
    args: Vec<String>,
    lookup: &impl Fn(&str) -> Option<String>,
    engine: Option<EngineBinding>,
) -> Result<()> {
    let home = home_dir(lookup)?;
    let config = Config::parse_from(args.clone(), lookup)?;
    let bind = config.bind;
    if bind.port() == 0 {
        bail!("`start` needs a fixed --bind port; port 0 is only valid for `serve`");
    }
    let dirs = Dirs::new(&home);
    dirs.create()?;
    let state_file = state_path(&dirs.run, bind.port());
    if let Some(existing) = read_state(&state_file)? {
        let comm = process_comm(existing.pid);
        let pid_start = process_start(existing.pid);
        match start_decision(Some(&existing), comm.as_deref(), pid_start.as_deref()) {
            StartDecision::AlreadyRunning { pid } => bail!(
                "gateway already running on {} (pid {pid}); stop it with `peri-browser stop --bind {}`",
                existing.bind,
                existing.bind
            ),
            StartDecision::CleanStale => {
                remove_state(&state_file)?;
                eprintln!(
                    "peri-browser: removed stale gateway state {}",
                    state_file.display()
                );
            }
            StartDecision::Start => {}
        }
    }
    // No state names this address: an unrecorded process answering `/healthz`
    // must not be mistaken for the child this start is about to spawn.
    if healthz_ok(bind).await {
        bail!(
            "http://{bind}/healthz already answers but no state file records it; \
             another process owns this address, so `start` will not adopt it; \
             choose another --bind or stop that process first"
        );
    }
    let log_path = dirs.logs.join(format!("gateway-{}.log", bind.port()));
    let mut child = spawn_gateway(&args, &log_path)?;
    let state = GatewayState {
        pid: child.id(),
        pid_start: process_start(child.id()),
        bind: bind.to_string(),
        log: log_path.display().to_string(),
        engine_port: engine.map(|binding| binding.port),
        engine_owned: engine.is_some_and(|binding| binding.owned),
        started_at: unix_seconds(),
    };
    // The state is published only after this child answers `/healthz` as
    // itself, so a concurrent start cannot see a half-ready gateway.
    if let Err(error) = wait_for_health(&mut child, bind, &log_path).await {
        let exited = terminate(&mut child).await;
        if !exited {
            // The child ignored SIGTERM: keep the pid discoverable for `stop`
            // instead of losing the only handle to it.
            match publish_state(&state_file, &state) {
                Ok(PublishOutcome::Published) => eprintln!(
                    "peri-browser: gateway pid {} did not exit after {}s; state recorded at {}",
                    state.pid,
                    STOP_LIMIT.as_secs(),
                    state_file.display()
                ),
                Ok(PublishOutcome::AlreadyExists) => eprintln!(
                    "peri-browser: gateway pid {} did not exit after {}s; existing state {} was kept",
                    state.pid,
                    STOP_LIMIT.as_secs(),
                    state_file.display()
                ),
                Err(publish) => eprintln!(
                    "peri-browser: gateway pid {} did not exit after {}s and its state could not be recorded: {publish:#}",
                    state.pid,
                    STOP_LIMIT.as_secs()
                ),
            }
            eprintln!(
                "peri-browser: stop it with `peri-browser stop --bind {bind}` (pid {})",
                state.pid
            );
        }
        return Err(error);
    }
    publish_until_owned(&state_file, &state, &mut child, bind).await?;
    println!("mcp: http://{bind}/mcp");
    println!("monitor: http://{bind}/");
    println!("pid: {}", state.pid);
    println!("log: {}", log_path.display());
    println!("stop: peri-browser stop --bind {bind}");
    Ok(())
}

/// Publishes a healthy gateway's state without overwriting a concurrent start.
///
/// A stale winner is removed and the publish retried once; a live winner makes
/// this start terminate its own child and report the recorded pid. The state
/// file is never replaced or removed once another start has published it.
async fn publish_until_owned(
    state_file: &Path,
    state: &GatewayState,
    child: &mut Child,
    bind: SocketAddr,
) -> Result<()> {
    for _ in 0..2 {
        let outcome = match publish_state(state_file, state) {
            Ok(outcome) => outcome,
            Err(error) => {
                terminate(child).await;
                return Err(error);
            }
        };
        if outcome == PublishOutcome::Published {
            return Ok(());
        }
        let existing = match read_state(state_file) {
            Ok(existing) => existing,
            Err(error) => {
                terminate(child).await;
                return Err(error);
            }
        };
        let comm = existing.as_ref().and_then(|state| process_comm(state.pid));
        let pid_start = existing.as_ref().and_then(|state| process_start(state.pid));
        let Some(existing) = existing else {
            continue;
        };
        match start_decision(Some(&existing), comm.as_deref(), pid_start.as_deref()) {
            StartDecision::AlreadyRunning { pid } => {
                terminate(child).await;
                bail!(
                    "gateway already running on {} (pid {pid}); stop it with `peri-browser stop --bind {}`",
                    existing.bind,
                    existing.bind
                );
            }
            StartDecision::CleanStale => {
                if let Err(error) = remove_state(state_file) {
                    terminate(child).await;
                    return Err(error);
                }
                eprintln!(
                    "peri-browser: removed stale gateway state {}",
                    state_file.display()
                );
            }
            StartDecision::Start => {}
        }
    }
    terminate(child).await;
    bail!(
        "another `peri-browser start` is publishing {bind}; gateway pid {} was terminated; stop it with `peri-browser stop --bind {bind}`",
        state.pid
    )
}

/// `stop`: terminate the gateway recorded for `bind`, then an owned engine.
///
/// Missing or dead state is idempotent success. A live pid that is not
/// `peri-browser`, a process that ignores SIGTERM, and a failed
/// `kitesurf stop` all return 1 without discarding the evidence.
pub async fn stop(bind: SocketAddr, lookup: &impl Fn(&str) -> Option<String>) -> Result<i32> {
    let home = home_dir(lookup)?;
    let dirs = Dirs::new(&home);
    let state_file = state_path(&dirs.run, bind.port());
    let Some(state) = read_state(&state_file)? else {
        println!("gateway on {bind} is not running");
        return Ok(0);
    };
    let comm = process_comm(state.pid);
    let pid_start = process_start(state.pid);
    let stopped = match stop_decision(Some(&state), bind, comm.as_deref(), pid_start.as_deref()) {
        StopDecision::NotRunning => {
            // The pid is gone, but an owned engine may still be running; the
            // state stays until that cleanup succeeds so `stop` can be retried.
            println!("gateway on {bind} is not running");
            false
        }
        StopDecision::OtherBind => {
            // The state file is keyed by port; a live gateway for another
            // loopback address must keep its state and stay untouched.
            println!("gateway on {bind} is not running");
            return Ok(0);
        }
        StopDecision::Refuse { pid, comm } => {
            eprintln!(
                    "peri-browser: refusing to stop pid {pid} on {bind}: process is {comm}, not {GATEWAY_COMM}; state kept at {}",
                    state_file.display()
                );
            return Ok(1);
        }
        StopDecision::Terminate { pid } => {
            if let Err(error) = send_sigterm(pid) {
                eprintln!(
                    "peri-browser: cannot stop gateway pid {pid}: {error:#}; state kept at {}",
                    state_file.display()
                );
                return Ok(1);
            }
            if !wait_for_exit(pid, STOP_LIMIT).await {
                eprintln!(
                        "peri-browser: gateway pid {pid} on {bind} still running after {}s; state kept at {}",
                        STOP_LIMIT.as_secs(),
                        state_file.display()
                    );
                return Ok(1);
            }
            true
        }
    };
    let others = other_states(&dirs.run, &state_file);
    if let Some(port) = engine_stop_target(&state, &others) {
        if let Err(error) = stop_engine(port, lookup) {
            eprintln!(
                "peri-browser: gateway on {bind} {} (pid {}), but the kitesurf engine on port {port} was not stopped: {error:#}; state kept at {}",
                if stopped { "was stopped" } else { "is not running" },
                state.pid,
                state_file.display()
            );
            return Ok(1);
        }
        println!("kitesurf engine stopped on port {port}");
    }
    remove_state(&state_file)?;
    if stopped {
        println!("gateway on {bind} stopped (pid {})", state.pid);
    }
    Ok(0)
}

/// Directory pair under `$HOME/.peri/browser`.
#[derive(Debug)]
struct Dirs {
    run: PathBuf,
    logs: PathBuf,
}

impl Dirs {
    fn new(home: &Path) -> Self {
        let base = home.join(".peri").join("browser");
        Self {
            run: base.join("run"),
            logs: base.join("logs"),
        }
    }

    fn create(&self) -> Result<()> {
        create_private_dir(&self.run)?;
        create_private_dir(&self.logs)
    }
}

fn create_private_dir(path: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(path)
        .with_context(|| format!("create {}", path.display()))?;
    // `create` leaves an existing directory's mode alone; tighten it anyway.
    fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))
        .with_context(|| format!("restrict {}", path.display()))
}

fn home_dir(lookup: &impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
    lookup("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set; cannot locate $HOME/.peri/browser")
}

/// `run/gateway-<port>.json`.
pub fn state_path(run_dir: &Path, port: u16) -> PathBuf {
    run_dir.join(format!("gateway-{port}.json"))
}

/// Writes the state through a same-directory rename so readers never see half
/// a document. Used by tests and fixtures; `start` claims the file exclusively
/// through [`publish_state`].
pub fn write_state(path: &Path, state: &GatewayState) -> Result<()> {
    let body = serde_json::to_vec_pretty(state).context("encode gateway state")?;
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&temp, body).with_context(|| format!("write {}", temp.display()))?;
    fs::rename(&temp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Publishes a new state file only when nobody else published it first.
///
/// The document is written to a same-directory temporary file and linked into
/// place with `hard_link`, whose `AlreadyExists` result makes the claim
/// exclusive without a check-then-write race. The temporary file is removed
/// whether or not the claim succeeds.
pub fn publish_state(path: &Path, state: &GatewayState) -> Result<PublishOutcome> {
    let body = serde_json::to_vec_pretty(state).context("encode gateway state")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let temp = path.with_extension(format!("json.{}.{nonce}.tmp", std::process::id()));
    fs::write(&temp, body).with_context(|| format!("write {}", temp.display()))?;
    let outcome = match fs::hard_link(&temp, path) {
        Ok(()) => Ok(PublishOutcome::Published),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(PublishOutcome::AlreadyExists)
        }
        Err(error) => Err(error).with_context(|| format!("publish {}", path.display())),
    };
    let _ = fs::remove_file(&temp);
    outcome
}

/// Reads a state file; a missing file is `None`.
pub fn read_state(path: &Path) -> Result<Option<GatewayState>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let state = serde_json::from_str(&text)
        .with_context(|| format!("parse gateway state {}", path.display()))?;
    Ok(Some(state))
}

/// Removes a state file; a missing file is success.
pub fn remove_state(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

/// Pure `start` arbitration: only a live gateway blocks a new start.
pub fn start_decision(
    state: Option<&GatewayState>,
    comm: Option<&str>,
    pid_start: Option<&str>,
) -> StartDecision {
    match state {
        None => StartDecision::Start,
        Some(state) => match comm {
            Some(comm) if is_gateway_comm(comm) && same_process(state, pid_start) => {
                StartDecision::AlreadyRunning { pid: state.pid }
            }
            _ => StartDecision::CleanStale,
        },
    }
}

/// Pure `stop` arbitration: never signal a pid that is not `peri-browser`, and
/// never cross to the other loopback address that shares a port.
pub fn stop_decision(
    state: Option<&GatewayState>,
    bind: SocketAddr,
    comm: Option<&str>,
    pid_start: Option<&str>,
) -> StopDecision {
    let (Some(state), Some(comm)) = (state, comm) else {
        return StopDecision::NotRunning;
    };
    if !state_matches_bind(state, bind) {
        return StopDecision::OtherBind;
    }
    if !same_process(state, pid_start) {
        // The pid was reused by something whose start identity differs.
        return StopDecision::NotRunning;
    }
    if is_gateway_comm(comm) {
        StopDecision::Terminate { pid: state.pid }
    } else {
        StopDecision::Refuse {
            pid: state.pid,
            comm: comm.to_owned(),
        }
    }
}

/// OS start identity decides which process a pid refers to.
///
/// When both the recorded and current identities exist they must match. Old
/// state files without `pid_start` or a failed lookup fall back to the `comm`
/// basename so the daemon keeps working across upgrades.
fn same_process(state: &GatewayState, current: Option<&str>) -> bool {
    match (state.pid_start.as_deref(), current) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => true,
    }
}

/// Whether a state file belongs to `bind`.
pub fn state_matches_bind(state: &GatewayState, bind: SocketAddr) -> bool {
    state
        .bind
        .parse::<SocketAddr>()
        .is_ok_and(|recorded| recorded == bind)
}

/// Engines stop only when this gateway owned the engine and no other state
/// file references the same port.
pub fn engine_stop_target(state: &GatewayState, others: &[GatewayState]) -> Option<u16> {
    let port = state.engine_port?;
    if !state.engine_owned {
        return None;
    }
    if others.iter().any(|other| other.engine_port == Some(port)) {
        return None;
    }
    Some(port)
}

fn is_gateway_comm(comm: &str) -> bool {
    Path::new(comm).file_name().and_then(|name| name.to_str()) == Some(GATEWAY_COMM)
}

/// Process name for `pid`; `None` when the pid does not exist.
#[cfg(target_os = "linux")]
fn process_comm(pid: u32) -> Option<String> {
    let comm = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let comm = comm.trim();
    (!comm.is_empty()).then(|| comm.to_owned())
}

/// Process name for `pid`; `None` when the pid does not exist.
#[cfg(not(target_os = "linux"))]
fn process_comm(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let comm = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!comm.is_empty()).then_some(comm)
}

/// OS-level process start identity, used to tell a reused pid apart from the
/// gateway that recorded it. `None` when the process is gone or unreadable.
#[cfg(target_os = "linux")]
fn process_start(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` may contain spaces and parentheses, so field splitting starts
    // after the last `)`. There, index 0 is field 3 (`state`) and field 22
    // (`starttime`) lands at index 19.
    let rest = stat.rsplit_once(')')?.1;
    rest.split_whitespace().nth(19).map(str::to_owned)
}

/// OS-level process start identity, used to tell a reused pid apart from the
/// gateway that recorded it. `None` when the process is gone or unreadable.
#[cfg(not(target_os = "linux"))]
fn process_start(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let start = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!start.is_empty()).then_some(start)
}

fn spawn_gateway(args: &[String], log: &Path) -> Result<Child> {
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("open {}", log.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("duplicate {}", log.display()))?;
    let exe = std::env::current_exe().context("resolve the peri-browser executable")?;
    let mut command = Command::new(&exe);
    command
        .arg("serve")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .process_group(0);
    command
        .spawn()
        .with_context(|| format!("start {}", exe.display()))
}

async fn wait_for_health(child: &mut Child, bind: SocketAddr, log: &Path) -> Result<()> {
    let deadline = Instant::now() + HEALTH_LIMIT;
    loop {
        if let Some(status) = child.try_wait().context("check the gateway process")? {
            bail!(
                "gateway exited before becoming healthy ({status}); see {}",
                log.display()
            );
        }
        // A different live gateway on the same bind must not satisfy this
        // start: only the child's own pid counts as ready.
        if healthz_pid(bind).await == Some(child.id()) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let pid = child.id();
            bail!(
                "gateway pid {pid} did not answer http://{bind}/healthz as itself within {}s; see {}",
                HEALTH_LIMIT.as_secs(),
                log.display()
            );
        }
        tokio::time::sleep(HEALTH_POLL).await;
    }
}

/// Sends SIGTERM and waits up to [`STOP_LIMIT`]; never escalates to SIGKILL.
/// Returns `true` only when the child is confirmed exited.
async fn terminate(child: &mut Child) -> bool {
    // `try_wait` may have already reaped the child; signalling a reaped pid
    // could hit a pid the OS handed to someone else.
    match child.try_wait() {
        Ok(Some(_)) => return true,
        Err(_) => return false,
        Ok(None) => {}
    }
    let _ = send_sigterm(child.id());
    let deadline = Instant::now() + STOP_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Err(_) => return false,
            Ok(None) if Instant::now() >= deadline => return false,
            Ok(None) => tokio::time::sleep(STOP_POLL).await,
        }
    }
}

fn send_sigterm(pid: u32) -> Result<()> {
    let target = pid.to_string();
    let mut last_error = None;
    for program in ["/bin/kill", "kill"] {
        match Command::new(program).args(["-TERM", &target]).status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => last_error = Some(anyhow::anyhow!("{program} reported {status}")),
            Err(error) => last_error = Some(error.into()),
        }
    }
    // The process may have exited between the comm check and the signal.
    if process_comm(pid).is_none() {
        return Ok(());
    }
    let error = last_error.unwrap_or_else(|| anyhow::anyhow!("no kill command available"));
    bail!("cannot send SIGTERM to pid {pid}: {error}")
}

async fn wait_for_exit(pid: u32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if process_comm(pid).is_none() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(STOP_POLL).await;
    }
}

/// State files other than `current` that still exist under `run/`.
fn other_states(run_dir: &Path, current: &Path) -> Vec<GatewayState> {
    let Ok(entries) = fs::read_dir(run_dir) else {
        return Vec::new();
    };
    let mut states = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == current || !is_state_file(&path) {
            continue;
        }
        match read_state(&path) {
            Ok(Some(state)) => states.push(state),
            Ok(None) => {}
            Err(error) => eprintln!(
                "peri-browser: ignoring unreadable state {}: {error:#}",
                path.display()
            ),
        }
    }
    states
}

fn is_state_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("gateway-") && name.ends_with(".json"))
}

fn stop_engine(port: u16, lookup: &impl Fn(&str) -> Option<String>) -> Result<()> {
    let binary = install_root(lookup)
        .map(|root| detect_install(&root, lookup))
        .and_then(|info| info.path)
        .unwrap_or_else(|| PathBuf::from("kitesurf"));
    let status = Command::new(&binary)
        .args(["stop", "--port", &port.to_string()])
        .status()
        .with_context(|| format!("run {} stop --port {port}", binary.display()))?;
    if !status.success() {
        bail!(
            "`{} stop --port {port}` failed ({status})",
            binary.display()
        );
    }
    Ok(())
}

/// A `/healthz` reply that passed the status check.
#[derive(Debug, Clone, Copy)]
struct HealthAnswer {
    pid: Option<u32>,
}

async fn healthz_ok(bind: SocketAddr) -> bool {
    healthz_answer(bind).await.is_some()
}

async fn healthz_pid(bind: SocketAddr) -> Option<u32> {
    healthz_answer(bind).await?.pid
}

async fn healthz_answer(bind: SocketAddr) -> Option<HealthAnswer> {
    tokio::time::timeout(HEALTH_ATTEMPT, healthz_attempt(bind))
        .await
        .ok()
        .flatten()
}

/// Reads the whole `/healthz` reply. The request asks for `Connection: close`,
/// so the body is read to EOF instead of assuming a fixed header length.
async fn healthz_attempt(bind: SocketAddr) -> Option<HealthAnswer> {
    let mut stream = TcpStream::connect(bind).await.ok()?;
    let request = format!("GET /healthz HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.ok()?;
    let mut response = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
        if response.len() > HEALTH_RESPONSE_LIMIT {
            return None;
        }
    }
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&response[..split]).ok()?;
    let status = head.lines().next()?;
    if status.split_whitespace().nth(1) != Some("200") {
        return None;
    }
    let pid = serde_json::from_slice::<serde_json::Value>(&response[split + 4..])
        .ok()
        .and_then(|body| body.get("pid").and_then(|pid| pid.as_u64()))
        .and_then(|pid| u32::try_from(pid).ok());
    Some(HealthAnswer { pid })
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
#[path = "daemon_test.rs"]
mod tests;
