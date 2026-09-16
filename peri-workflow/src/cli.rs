//! Stable wrapper for the embedded workflow engine CLI.

use std::ffi::OsString;
use std::process::Stdio;

use crate::error::WorkflowError;

/// Run an allowlisted workflow CLI command in an isolated temporary artifact directory.
///
/// The wrapper deliberately starts node directly. It never resolves or downloads a
/// package, and the temporary directory is retained until the child exits.
pub async fn run(args: &[OsString]) -> Result<i32, WorkflowError> {
    validate_args(args)?;

    let temp = tempfile::Builder::new()
        .prefix("peri-workflow-cli-")
        .tempdir()?;
    let artifact = temp.path().join("peri-workflow.js");
    tokio::fs::write(&artifact, crate::runner::WORKFLOW_ARTIFACT_BYTES).await?;

    let status = tokio::process::Command::new("node")
        .arg(&artifact)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|error| WorkflowError::SpawnFailed(format!("failed to run node: {error}")))?;

    Ok(status.code().unwrap_or(1))
}

/// Validate the stable top-level command allowlist before spawning Node.
fn validate_args(args: &[OsString]) -> Result<(), WorkflowError> {
    let Some(command) = args.first().and_then(|arg| arg.to_str()) else {
        return Err(WorkflowError::SpawnFailed(
            "workflow CLI requires a subcommand".into(),
        ));
    };
    let allowed = matches!(
        command,
        "read" | "list" | "validate" | "boundary" | "adlc" | "help" | "-h" | "--help"
    );
    if allowed {
        Ok(())
    } else {
        Err(WorkflowError::SpawnFailed(format!(
            "unsupported workflow CLI subcommand: {command}"
        )))
    }
}

#[cfg(test)]
#[path = "cli_test.rs"]
mod tests;
