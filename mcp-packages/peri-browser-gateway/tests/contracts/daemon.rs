//! `start`/`stop` contract: the command returns after a health check, the
//! state file tracks the bind, and `stop` releases the port idempotently.

use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::support::fake_engine::Engine;

const BINARY: &str = env!("CARGO_BIN_EXE_peri-browser");
/// Deterministic PATH for the `ps` and `kill` lookups used by process checks.
const PATH_ENV: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const START_LIMIT: Duration = Duration::from_secs(15);
const STOP_LIMIT: Duration = Duration::from_secs(5);

struct Daemon {
    home: tempfile::TempDir,
    port: u16,
}

impl Daemon {
    fn new() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        Ok(Self {
            home: tempfile::tempdir()?,
            port,
        })
    }

    fn bind(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn state_path(&self) -> PathBuf {
        self.home
            .path()
            .join(".peri/browser/run")
            .join(format!("gateway-{}.json", self.port))
    }

    fn log_path(&self) -> PathBuf {
        self.home
            .path()
            .join(".peri/browser/logs")
            .join(format!("gateway-{}.log", self.port))
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BINARY);
        command
            .args(args)
            .env_clear()
            .env("HOME", self.home.path())
            .env("PATH", PATH_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, args: &[&str], limit: Duration) -> anyhow::Result<(ExitStatus, String, String)> {
        let child = self.command(args).spawn()?;
        finish(child, limit, &args.join(" "))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // The guard only uses the product's own stop path, which refuses a pid
        // whose comm is not `peri-browser`.
        let _ = self.command(&["stop", "--bind", &self.bind()]).output();
        if self.state_path().exists() {
            eprintln!(
                "daemon test cleanup left state {}",
                self.state_path().display()
            );
        }
    }
}

fn finish(
    mut child: Child,
    limit: Duration,
    what: &str,
) -> anyhow::Result<(ExitStatus, String, String)> {
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{what} did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .context("capture stdout")?
        .read_to_string(&mut stdout)?;
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .context("capture stderr")?
        .read_to_string(&mut stderr)?;
    Ok((status, stdout, stderr))
}

async fn wait_for_port_release(bind: &str, limit: Duration) -> anyhow::Result<()> {
    let addr: SocketAddr = bind.parse()?;
    let deadline = Instant::now() + limit;
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_err() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("{bind} still accepts connections after {limit:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn start_returns_after_the_health_check_and_stop_is_idempotent() -> anyhow::Result<()> {
    let daemon = Daemon::new()?;
    let bind = daemon.bind();

    let (status, stdout, stderr) = daemon.run(&["start", "--bind", &bind], START_LIMIT)?;
    assert!(status.success(), "start failed: {stderr}");
    assert!(
        stdout.contains(&format!("mcp: http://{bind}/mcp")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("monitor: http://{bind}/")),
        "{stdout}"
    );
    assert!(stdout.contains("pid: "), "{stdout}");
    assert!(
        stdout.contains(&format!("log: {}", daemon.log_path().display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("peri-browser stop --bind {bind}")),
        "{stdout}"
    );

    let state_text = std::fs::read_to_string(daemon.state_path())?;
    let state: serde_json::Value = serde_json::from_str(&state_text)?;
    let pid = state["pid"].as_u64().context("state pid")?;
    assert!(stdout.contains(&format!("pid: {pid}")), "{stdout}");
    assert_eq!(state["bind"], bind.as_str());
    assert_eq!(state["log"], daemon.log_path().display().to_string());
    assert_eq!(state["engine_port"], serde_json::Value::Null);
    assert_eq!(state["engine_owned"], false);
    assert!(state["started_at"].as_u64().unwrap_or(0) > 0);
    assert!(
        state["pid_start"]
            .as_str()
            .is_some_and(|start| !start.trim().is_empty()),
        "start must record the OS process start identity: {state_text}"
    );
    assert!(daemon.log_path().exists());

    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let health: serde_json::Value = http
        .get(format!("http://{bind}/healthz"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(health["ok"], true);
    assert_eq!(
        health["pid"], pid,
        "/healthz must identify the gateway the state records"
    );

    // A live gateway blocks a second start and points at its pid and stop.
    let (status, _, stderr) = daemon.run(&["start", "--bind", &bind], START_LIMIT)?;
    assert!(!status.success(), "second start must fail: {stderr}");
    assert!(stderr.contains(&format!("pid {pid}")), "{stderr}");
    assert!(stderr.contains("peri-browser stop"), "{stderr}");
    assert!(daemon.state_path().exists());
    let after_conflict: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(daemon.state_path())?)?;
    assert_eq!(
        after_conflict, state,
        "a losing start must not overwrite the winner's state"
    );

    let (status, stdout, stderr) = daemon.run(&["stop", "--bind", &bind], STOP_LIMIT)?;
    assert!(status.success(), "stop failed: {stderr}");
    assert!(stdout.contains("stopped"), "{stdout}");
    assert!(
        !daemon.state_path().exists(),
        "stop must remove {}",
        daemon.state_path().display()
    );
    wait_for_port_release(&bind, STOP_LIMIT).await?;

    let (status, stdout, stderr) = daemon.run(&["stop", "--bind", &bind], STOP_LIMIT)?;
    assert!(status.success(), "second stop must be idempotent: {stderr}");
    assert!(stdout.contains("not running"), "{stdout}");
    Ok(())
}

/// A reachable engine is reused: the state records its port, `engine_owned`
/// stays false, and `stop` leaves the engine alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_kitesurf_does_not_own_a_reused_engine() -> anyhow::Result<()> {
    let mut engine = Engine::ephemeral().await?;
    let daemon = Daemon::new()?;
    let bind = daemon.bind();
    let port = engine.addr.port().to_string();

    let (status, stdout, stderr) = daemon.run(
        &["start", "kitesurf", "--port", &port, "--bind", &bind],
        START_LIMIT,
    )?;
    assert!(status.success(), "start kitesurf failed: {stderr}");
    assert!(
        stdout.contains(&format!(
            "kitesurf already running at http://127.0.0.1:{port}"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("mcp: http://{bind}/mcp")),
        "{stdout}"
    );

    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(daemon.state_path())?)?;
    assert_eq!(state["engine_port"], port.parse::<u16>().unwrap());
    assert_eq!(state["engine_owned"], false);

    let (status, _, stderr) = daemon.run(&["stop", "--bind", &bind], STOP_LIMIT)?;
    assert!(status.success(), "stop failed: {stderr}");
    assert!(!daemon.state_path().exists());

    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let health = http
        .get(format!("{}/json/version", engine.url()))
        .send()
        .await?
        .error_for_status()?;
    assert!(health.json::<serde_json::Value>().await?.is_object());
    engine.shutdown().await
}
