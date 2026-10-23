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

/// Mainland China mirror documented in README's "Get started" section.
const GITHUB_PROXY: &str = "https://github-proxy-5ad00a.netlify.app";
const GITHUB_API_PROXY: &str = "https://github-proxy-5ad00a.netlify.app/api";
const SCRIPT_URL_SH_PROXIED: &str =
    "https://github-proxy-5ad00a.netlify.app/konghayao/peri/raw/main/scripts/install.sh";
const SCRIPT_URL_PS1_PROXIED: &str =
    "https://github-proxy-5ad00a.netlify.app/konghayao/peri/raw/main/scripts/install.ps1";

/// Install-script URL for the current platform.
fn script_url(use_proxy: bool, windows: bool) -> &'static str {
    match (windows, use_proxy) {
        (false, false) => SCRIPT_URL_SH,
        (false, true) => SCRIPT_URL_SH_PROXIED,
        (true, false) => SCRIPT_URL_PS1,
        (true, true) => SCRIPT_URL_PS1_PROXIED,
    }
}

/// Proxy variables consumed by the install scripts for release downloads
/// and GitHub API queries. Empty when the proxy is not requested, so the
/// scripts keep their direct behaviour and still inherit any user-set values.
fn proxy_env(use_proxy: bool) -> &'static [(&'static str, &'static str)] {
    if use_proxy {
        &[
            ("GITHUB_PROXY", GITHUB_PROXY),
            ("GITHUB_API_PROXY", GITHUB_API_PROXY),
        ]
    } else {
        &[]
    }
}

/// Run the update flow. Returns Ok(new_tag) on success.
///
/// Streams the remote install script's stdout/stderr to the terminal.
pub async fn run_update(use_proxy: bool) -> Result<String> {
    println!("Peri update");

    if use_proxy {
        println!("  Using GitHub proxy: {GITHUB_PROXY}");
    }

    if cfg!(target_os = "windows") {
        run_update_windows(use_proxy).await
    } else {
        run_update_unix(use_proxy).await
    }
}

async fn run_update_unix(use_proxy: bool) -> Result<String> {
    println!("  Running remote install script...");

    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!(
            "curl -fsSL {} | bash",
            script_url(use_proxy, false)
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in proxy_env(use_proxy) {
        command.env(key, value);
    }

    let mut child = command
        .spawn()
        .context("Failed to spawn update process. Is bash/curl available?")?;

    stream_output(&mut child).await?;
    read_installed_version()
}

async fn run_update_windows(use_proxy: bool) -> Result<String> {
    println!("  Running remote install script...");

    let ps_command = format!("irm {} | iex", script_url(use_proxy, true));

    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &ps_command,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in proxy_env(use_proxy) {
        command.env(key, value);
    }

    let mut child = command
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

#[cfg(test)]
#[path = "update_test.rs"]
mod tests;
