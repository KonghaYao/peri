//! npm 安装子进程 owner：取消/安装超时均先回收进程树，再 join stderr。

use std::{io, path::Path, process::Stdio};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::{npm_command, Installer, INSTALL_TIMEOUT, MAX_INSTALL_STDERR_BYTES};
use peri_process::ProcessTree;

pub(super) struct NpmInstaller;

#[async_trait::async_trait]
impl Installer for NpmInstaller {
    async fn install(&self, staging: &Path, cancel: &CancellationToken) -> io::Result<bool> {
        let home = staging.join(".npm-home");
        let cache = staging.join(".npm-cache");
        tokio::fs::create_dir(&home).await?;
        tokio::fs::create_dir(&cache).await?;
        run_install(npm_command(staging, &home, &cache), cancel).await
    }
}

struct InstallProcess {
    child: Child,
    tree: ProcessTree,
    stderr: JoinHandle<io::Result<Vec<u8>>>,
}

impl InstallProcess {
    async fn spawn(mut command: Command) -> io::Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut tree = ProcessTree::new()?;
        tree.prepare(&mut command);
        let mut child = command.spawn()?;
        if let Err(error) = tree.attach(&child) {
            tree.terminate();
            let _ = child.wait().await;
            tree.wait_for_exit().await;
            return Err(error);
        }
        let mut stderr = child.stderr.take().expect("npm stderr configured as piped");
        let stderr = tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = stderr.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                tail.extend_from_slice(&buffer[..count]);
                if tail.len() > MAX_INSTALL_STDERR_BYTES {
                    tail.drain(..tail.len() - MAX_INSTALL_STDERR_BYTES);
                }
            }
            Ok(tail)
        });
        Ok(Self {
            child,
            tree,
            stderr,
        })
    }

    async fn finish(&mut self) -> io::Result<Vec<u8>> {
        self.tree.terminate();
        let reaped = self.child.wait().await;
        self.tree.wait_for_exit().await;
        let stderr = (&mut self.stderr)
            .await
            .map_err(|_| io::Error::other("npm stderr task failed"))?;
        reaped?;
        stderr
    }
}

impl Drop for InstallProcess {
    fn drop(&mut self) {
        self.stderr.abort();
        let _ = self.child.start_kill();
        // ProcessTree::drop is the cancellation fail-safe if the owner future is dropped.
    }
}

async fn run_install(command: Command, cancel: &CancellationToken) -> io::Result<bool> {
    if cancel.is_cancelled() {
        return Err(io::Error::from(io::ErrorKind::Interrupted));
    }
    let mut process = InstallProcess::spawn(command).await?;
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(io::Error::from(io::ErrorKind::Interrupted)),
        _ = tokio::time::sleep(INSTALL_TIMEOUT) => Ok(false),
        result = process.child.wait() => result.map(|status| status.success()),
    };
    let stderr = process.finish().await?;
    if matches!(result, Ok(false)) {
        debug!(
            stderr_tail_bytes = stderr.len(),
            "PTC npm install failed or timed out"
        );
    }
    result
}

#[cfg(all(test, unix))]
#[path = "install_test.rs"]
mod tests;
