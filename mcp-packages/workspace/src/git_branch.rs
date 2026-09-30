use std::{process::Stdio, time::Duration};

use peri_mcp_common::shell::ShellExecutionGuard;
use rmcp::{model::CustomResult, ErrorData as McpError};
use tokio::io::AsyncReadExt;

const GIT_BRANCH_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) async fn read_branch(cwd: &str) -> Result<CustomResult, McpError> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd);
    let branch = current_branch_with_command(command, GIT_BRANCH_TIMEOUT).await;
    Ok(CustomResult::new(serde_json::json!({ "branch": branch })))
}

fn spawn_branch_command(mut command: tokio::process::Command) -> Option<ShellExecutionGuard> {
    let mut guard = ShellExecutionGuard::new(None);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    guard.prepare(&mut command).ok()?;
    guard.attach_owned(command.spawn().ok()?).ok()?;
    Some(guard)
}

async fn current_branch_from_child(
    mut guard: ShellExecutionGuard,
    timeout: Duration,
) -> Option<String> {
    let mut stdout = guard.child_mut().stdout.take()?;
    let mut bytes = Vec::new();
    let status = tokio::time::timeout(timeout, async {
        let (status, _) =
            tokio::try_join!(guard.child_mut().wait(), stdout.read_to_end(&mut bytes),)?;
        Ok::<_, std::io::Error>(status)
    })
    .await
    .ok()?
    .ok()?;
    if !status.success() {
        return None;
    }
    let branch = String::from_utf8(bytes).ok()?;
    let branch = branch.trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

async fn current_branch_with_command(
    command: tokio::process::Command,
    timeout: Duration,
) -> Option<String> {
    current_branch_from_child(spawn_branch_command(command)?, timeout).await
}

#[cfg(test)]
#[path = "git_branch_test.rs"]
mod tests;
