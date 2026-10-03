//! Brokered one-shot Workflow subprocesses.

use std::io;
use std::process::{ExitStatus, Output};

use peri_process::ProcessTree;
use tokio::process::{Child, Command};

pub(super) fn spawn(mut command: Command) -> io::Result<(Child, ProcessTree)> {
    let mut tree = ProcessTree::new()?;
    tree.prepare(&mut command);
    let mut child = command.spawn()?;
    if let Err(error) = tree.attach(&child) {
        tree.terminate();
        // This exceptional path cannot await; kill_on_drop and tree Drop hold
        // the group until the parent process is reaped by Tokio.
        let _ = child.start_kill();
        return Err(error);
    }
    Ok((child, tree))
}

pub(super) async fn output(mut command: Command) -> io::Result<Output> {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (child, tree) = spawn(command)?;
    let output = child.wait_with_output().await;
    if output.is_err() {
        tree.terminate();
    }
    tree.wait_for_exit().await;
    output
}

pub(super) async fn status(command: Command) -> io::Result<ExitStatus> {
    let (mut child, tree) = spawn(command)?;
    let status = child.wait().await;
    if status.is_err() {
        tree.terminate();
    }
    tree.wait_for_exit().await;
    status
}
