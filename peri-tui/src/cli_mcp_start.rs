//! Dispatches an MCP capability before Peri configuration or Agent startup.

use std::{ffi::OsString, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};

const WORKSPACE_BINARY: &str = if cfg!(windows) {
    "peri-mcp-workspace.exe"
} else {
    "peri-mcp-workspace"
};

fn worker_path(peri_executable: PathBuf) -> Result<PathBuf> {
    let directory = peri_executable
        .parent()
        .context("cannot locate the Peri executable directory")?;
    let worker = directory.join(WORKSPACE_BINARY);
    if !worker.is_file() {
        bail!(
            "Workspace MCP executable is missing from the Peri installation: {}",
            worker.display()
        );
    }
    Ok(worker)
}

pub(super) fn run(args: &[OsString]) -> Result<()> {
    let Some(capability) = args.first() else {
        bail!("expected an MCP capability: peri mcp-start workspace --stdio|--http");
    };
    if capability != "workspace" {
        bail!("unknown MCP capability: {}", capability.to_string_lossy());
    }
    let executable = std::env::current_exe().context("cannot locate the Peri executable")?;
    let worker = worker_path(executable)?;
    let mut command = Command::new(worker);
    command.args(&args[1..]);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = command.status().context("cannot start Workspace MCP")?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
#[path = "cli_mcp_start_test.rs"]
mod tests;
