//! Shared OS browser opener for OAuth authorization URLs.
//!
//! Two call sites use it: the session MCP OAuth popup and ChatGPT sign-in.
//! The authorization URL contains `state`/`nonce` and must never be logged, so
//! this module logs only the launcher program.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Upper bound for reaping the launcher process. Browsers commonly exit
/// immediately after handing the URL to an already-running instance; a stale
/// launcher must not accumulate.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// Build the platform launcher and its arguments.
///
/// Arguments are passed directly to the program (no shell interpolation).
pub(crate) fn browser_command_for(os: &str, url: &str) -> (&'static str, Vec<String>) {
    match os {
        "macos" => ("open", vec![url.to_owned()]),
        "windows" => (
            "cmd",
            vec![
                "/C".to_owned(),
                "start".to_owned(),
                String::new(),
                url.to_owned(),
            ],
        ),
        _ => ("xdg-open", vec![url.to_owned()]),
    }
}

fn current_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

/// Launch the system browser and reap it in a bounded background thread.
///
/// Returns as soon as the launcher starts. `Ok(())` means the OS command was
/// started, never that authorization succeeded.
pub(crate) fn open_browser(url: &str) -> Result<(), String> {
    let (program, args) = browser_command_for(current_os(), url);
    let mut command = Command::new(program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    std::thread::spawn(move || reap(&mut child));
    tracing::debug!(program, "requested system browser");
    Ok(())
}

fn reap(child: &mut Child) {
    let deadline = Instant::now() + REAP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!("browser launcher did not exit before the reap deadline");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to reap browser launcher");
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "auth_browser_test.rs"]
mod tests;
