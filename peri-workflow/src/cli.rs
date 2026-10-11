//! Stable wrapper for the workflow engine CLI.

use std::ffi::OsString;
use std::process::Stdio;

use crate::error::WorkflowError;

/// Run an allowlisted workflow CLI command against the validated local engine artifact.
///
/// Acquisition goes through `prepare_workflow_command`: the pinned package installed under
/// `~/.peri/workflow/<version>/` is reused offline, with an explicit opt-in npx fallback.
/// The engine source is never copied into or embedded in the Rust binary.
pub async fn run(args: &[OsString]) -> Result<i32, WorkflowError> {
    validate_args(args)?;

    let prepared = crate::runner::prepare_workflow_command().await?;

    let mut command = tokio::process::Command::new(&prepared.program);
    command
        .args(&prepared.args)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let status = crate::process::status(command)
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
