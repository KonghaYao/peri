//! Update mechanism: downloads and runs the remote install script.
//!
//! On Unix: curl install.sh | bash
//! On Windows: irm install.ps1 | iex
//!
//! Delegates all update logic (download, checksum, extract, symlink)
//! to the remote install scripts.

use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

const SCRIPT_URL_SH: &str =
    "https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.sh";
const SCRIPT_URL_PS1: &str =
    "https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.ps1";

/// Run the update flow. Returns Ok(new_tag) on success.
///
/// Streams the remote install script's stdout/stderr to the terminal.
pub async fn run_update() -> Result<String> {
    println!("Peri update");

    if cfg!(target_os = "windows") {
        run_update_windows().await
    } else {
        run_update_unix().await
    }
}

async fn run_update_unix() -> Result<String> {
    println!("  Running remote install script...");

    let mut child = Command::new("bash")
        .arg("-c")
        .arg(format!("curl -fsSL {SCRIPT_URL_SH} | bash"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn update process. Is bash/curl available?")?;

    stream_output(&mut child).await?;
    read_installed_version()
}

async fn run_update_windows() -> Result<String> {
    println!("  Running remote install script...");

    let ps_command = format!("irm {SCRIPT_URL_PS1} | iex");

    let mut child = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &ps_command,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn update process. Is PowerShell available?")?;

    stream_output(&mut child).await?;
    read_installed_version()
}

async fn stream_output(child: &mut tokio::process::Child) -> Result<()> {
    // 流式输出 stdout
    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            println!("{line}");
        }
    }

    // 流式输出 stderr
    if let Some(stderr) = child.stderr.take() {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            eprintln!("{line}");
        }
    }

    let status = child.wait().await?;
    if !status.success() {
        anyhow::bail!("Update script exited with status {}", status);
    }

    Ok(())
}

fn read_installed_version() -> Result<String> {
    let version_file = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".peri")
        .join("current-version.txt");
    let tag = std::fs::read_to_string(&version_file)
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    Ok(tag)
}
