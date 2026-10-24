//! Update mechanism: downloads and runs the remote install script.
//!
//! On Unix: download install.sh with curl, then run it with bash via stdin
//! On Windows: download install.ps1 with Invoke-RestMethod and invoke it
//!
//! Delegates all update logic (download, checksum, extract, symlink)
//! to the remote install scripts.

use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
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

/// Environment applied to the install-script child process.
struct InstallerEnv {
    vars: &'static [(&'static str, &'static str)],
    drop_github_token: bool,
}

/// Proxy variables consumed by the install scripts for release downloads
/// and GitHub API queries. Empty when the proxy is not requested, so the
/// scripts keep their direct behaviour and still inherit any user-set values.
///
/// The proxied API URL belongs to a third-party mirror, so the user's
/// `GITHUB_TOKEN` must not reach it; direct installs keep the token for
/// GitHub API rate-limit bypass.
fn installer_env(use_proxy: bool) -> InstallerEnv {
    if use_proxy {
        InstallerEnv {
            vars: &[
                ("GITHUB_PROXY", GITHUB_PROXY),
                ("GITHUB_API_PROXY", GITHUB_API_PROXY),
            ],
            drop_github_token: true,
        }
    } else {
        InstallerEnv {
            vars: &[],
            drop_github_token: false,
        }
    }
}

fn apply_installer_env(command: &mut Command, use_proxy: bool) {
    let env = installer_env(use_proxy);
    for (key, value) in env.vars {
        command.env(key, value);
    }
    if env.drop_github_token {
        command.env_remove("GITHUB_TOKEN");
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

/// Download the install script before running it so a failed download is
/// reported instead of being masked by bash exiting successfully on empty input.
async fn fetch_install_script(url: &str) -> Result<Vec<u8>> {
    let output = Command::new("curl")
        .args(["-fsSL", url])
        .output()
        .await
        .with_context(|| format!("Failed to spawn curl to download {url}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            anyhow::bail!(
                "Failed to download install script from {url}: {}",
                output.status
            );
        }
        anyhow::bail!(
            "Failed to download install script from {url}: {} ({detail})",
            output.status
        );
    }
    if output.stdout.is_empty() {
        anyhow::bail!("Install script download returned no content: {url}");
    }

    Ok(output.stdout)
}

async fn run_update_unix(use_proxy: bool) -> Result<String> {
    println!("  Running remote install script...");

    let script = fetch_install_script(script_url(use_proxy, false)).await?;

    let mut command = Command::new("bash");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_installer_env(&mut command, use_proxy);

    let mut child = command
        .spawn()
        .context("Failed to spawn update process. Is bash available?")?;
    {
        let mut stdin = child
            .stdin
            .take()
            .context("Failed to open stdin for the install script")?;
        stdin
            .write_all(&script)
            .await
            .context("Failed to pass the install script to bash")?;
    }

    stream_output(&mut child).await?;
    read_installed_version()
}

async fn run_update_windows(use_proxy: bool) -> Result<String> {
    println!("  Running remote install script...");

    let ps_command = format!(
        "$ErrorActionPreference = 'Stop'; irm {} | iex",
        script_url(use_proxy, true)
    );

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
    apply_installer_env(&mut command, use_proxy);

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
